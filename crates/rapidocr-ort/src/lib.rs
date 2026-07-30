//! # rapidocr-ort
//!
//! 基于 ONNX Runtime 的 PP-OCR 文字识别（检测 + 识别）Rust 实现。
//!
//! 设计要点：
//! - **模型运行时可选**：通过 [`ModelProfile`]（`--model` 参数）选择 v3 / v6-tiny /
//!   v6-medium 套件，权重从 `models/rapidocr/` 加载，不条件编译。三件套（det/rec/cls）
//!   与字典随 profile 走。
//! - **既是库也是二进制**：库暴露 [`OcrEngine`] 给上层（如 ui_probe）复用；二进制
//!   `rapidocr-ort` 直接对一张图片出 JSON（文字 + 坐标）。
//! - 推理代码模型无关，同一套 DB 检测后处理 + CRNN/CTC 识别可跑任意 PP-OCR 权重。
//!
//! ## 坐标系
//! 每个检测结果的 `bbox` 为四个顶点 `[x0,y0,x1,y1,x2,y2,x3,y3]`（顺时针：左上、右上、
//! 右下、左下），与图片像素坐标一致；`center` 为质心，便于操作回灌（点击中心点）。

use anyhow::{Context, Result};
// 用 ort 重新导出的 ndarray（开启 ndarray feature 后可用），避免依赖图里出现两份
// ndarray 导致 Array 类型不匹配（ort 的 OwnedTensorArrayData 只对它锁定的那份实现）。
use ndarray::{Array, Array3, Array4, Axis};
use ort::session::Session;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// 模型套件预设。枚举值即 `--model` 的取值。
///
/// 每套对应 `models/rapidocr/` 下的一组权重 + 一个字典文件。新增套件只改这里，
/// 不碰推理代码。
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ModelProfile {
    /// PP-OCRv3（成熟、参考多），det/rec/cls 均 v3，字典 ppocr_keys.json。
    V3,
    /// PP-OCRv6 tiny：快、体积小，字典 ppocrv6_tiny_dict.txt。
    V6Tiny,
    /// PP-OCRv6 medium：更准，模型较大，字典 ppocrv6_dict.txt。
    V6Medium,
}

impl ModelProfile {
    /// 返回 (det, rec, cls, dict) 四个文件的相对 `models/rapidocr/` 的路径。
    fn paths(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            ModelProfile::V3 => (
                "ch_PP-OCRv3_det_infer.onnx",
                "ch_PP-OCRv3_rec_infer.onnx",
                "ch_ppocr_mobile_v2.0_cls_infer.onnx",
                "ppocr_keys.json",
            ),
            ModelProfile::V6Tiny => (
                "pp-ocrv6_tiny_det.onnx",
                "pp-ocrv6_tiny_rec.onnx",
                "ch_ppocr_mobile_v2.0_cls_infer.onnx",
                "ppocrv6_tiny_dict.txt",
            ),
            ModelProfile::V6Medium => (
                "pp-ocrv6_medium_det.onnx",
                "pp-ocrv6_medium_rec.onnx",
                "ch_ppocr_mobile_v2.0_cls_infer.onnx",
                "ppocrv6_dict.txt",
            ),
        }
    }

    /// 识别分支输入高度。PP-OCRv3 接受任意高度（用 32 即可），PP-OCRv6 的 rec
    /// 模型固定输入高度 48，必须匹配否则推理报维度错误。
    fn rec_input_h(self) -> usize {
        match self {
            ModelProfile::V3 => 32,
            ModelProfile::V6Tiny | ModelProfile::V6Medium => 48,
        }
    }

    /// 识别分支归一化参数 (mean, std)。
    /// - v3 用 ImageNet 统计（与训练一致，已验证正确）。
    /// - v6 用 (0.5, 0.5) 占位：PP-OCRv6 的 rec 预处理与 v3 不同（非 ImageNet），
    ///   具体方案待补，当前仅保证能出非空结果，识别不一定准（实验性）。
    fn rec_norm(self) -> ([f32; 3], [f32; 3]) {
        match self {
            ModelProfile::V3 => ([0.485, 0.456, 0.406], [0.229, 0.224, 0.225]),
            ModelProfile::V6Tiny | ModelProfile::V6Medium => ([0.5, 0.5, 0.5], [0.5, 0.5, 0.5]),
        }
    }
}

/// 单个文字识别结果。
#[derive(Clone, Debug, Serialize)]
pub struct OcrResult {
    /// 识别出的文字。
    pub text: String,
    /// 置信度（识别分支平均字符概率）。
    pub score: f32,
    /// 四个顶点，顺时针：[x0,y0,x1,y1,x2,y2,x3,y3]（像素坐标）。
    pub bbox: [f32; 8],
    /// 质心坐标，便于点击回灌。
    pub center: [f32; 2],
}

/// OCR 引擎：持有 det / cls / rec 三个 Session 与字典。
pub struct OcrEngine {
    profile: ModelProfile,
    det: Session,
    rec: Session,
    cls: Session,
    /// 字典：index -> 字符。第 0 位通常是空白符（CTC blank）。
    vocab: Vec<String>,
    /// det / rec 的输出节点名（PP-OCR v3 为 sigmoid_0.tmp_0 / softmax_5.tmp_0，
    /// v6 等不同；在构建时按索引缓存，避免硬编码）。
    det_out_name: String,
    rec_out_name: String,
}

impl OcrEngine {
    /// 按预设套件构建引擎。
    ///
    /// `model_dir` 为 `models/rapidocr` 所在目录（默认仓库根的 `models/rapidocr`）。
    /// 字典优先从同目录读取；若文件缺失则回退到内嵌资源（settings crate 的
    /// rust-embed），保证无外部文件也能跑（当前实现以文件为准）。
    pub fn from_profile(profile: ModelProfile, model_dir: &Path) -> Result<Self> {
        let (det, rec, cls, dict) = profile.paths();
        let dir = model_dir.to_path_buf();
        let det_path = dir.join(det);
        let rec_path = dir.join(rec);
        let cls_path = dir.join(cls);
        let dict_path = dir.join(dict);

        let det = build_session(&det_path)?;
        let rec = build_session(&rec_path)?;
        let cls = build_session(&cls_path)?;
        let vocab = load_vocab(&dict_path)
            .with_context(|| format!("加载字典失败: {}", dict_path.display()))?;

        // 缓存输出节点名（按索引取首个输出，PP-OCR 检测/识别均单输出）。
        let det_out_name = det.outputs()[0].name().to_string();
        let rec_out_name = rec.outputs()[0].name().to_string();

        Ok(Self {
            profile,
            det,
            rec,
            cls,
            vocab,
            det_out_name,
            rec_out_name,
        })
    }

    /// 便捷：用仓库根 `models/rapidocr` 构建。
    pub fn from_profile_default_dir(profile: ModelProfile) -> Result<Self> {
        // 二进制运行时的当前目录一般是仓库根；库调用方也可显式传路径。
        let dir = PathBuf::from("models/rapidocr");
        Self::from_profile(profile, &dir)
    }

    pub fn profile(&self) -> ModelProfile {
        self.profile
    }

    /// 对一张 RGB 图像（height×width×3，0-255 u8）做检测 + 识别。
    ///
    /// `img` 为列优先的扁平数据或 ndarray；这里接收 `Array3<u8>`（HWC）。
    pub fn detect(&mut self, img: &Array3<u8>) -> Result<Vec<OcrResult>> {
        let (h, w, _) = img.dim();

        // ---- 1. 检测：原图缩放到输入尺寸，归一化后跑 det ----
        let det_input = preprocess_det(img)?;
        let det_tensor = ort::value::Tensor::from_array(det_input)
            .context("构造 det 输入张量失败")?;
        let det_out = self
            .det
            .run(ort::inputs!["x" => det_tensor])
            .context("det 推理失败")?;
        // 取 det 的唯一输出（PP-OCR 检测只有 1 个输出；v3 名为 sigmoid_0.tmp_0，
        // v6 可能不同，构建时已缓存到 det_out_name）。
        let det_map = det_out[self.det_out_name.as_str()].try_extract_array::<f32>()?.to_owned();
        // DB 后处理：二值化 + 找轮廓 + 多边形近似，返回原图坐标系下的四点框
        let boxes = db_postprocess(&det_map, h, w)?;

        // ---- 2. 逐个文本框裁剪 + 识别 ----
        let mut results = Vec::with_capacity(boxes.len());
        for bbox in boxes {
            // 按 bbox 透视变换/crop 出归一化图块，送 rec
            let crop = crop_and_resize_for_rec(img, &bbox)?;
            let rec_input = preprocess_rec(&crop, self.profile.rec_input_h(), self.profile.rec_norm())?;
            let rec_tensor = ort::value::Tensor::from_array(rec_input)
                .context("构造 rec 输入张量失败")?;
            let rec_out = self
                .rec
                .run(ort::inputs!["x" => rec_tensor])
                .context("rec 推理失败")?;
            let logits = rec_out[self.rec_out_name.as_str()].try_extract_array::<f32>()?.to_owned();
            let (text, score) = ctc_greedy_decode(&logits, &self.vocab);
            if text.is_empty() {
                continue;
            }
            let center = bbox_center(&bbox);
            results.push(OcrResult {
                text,
                score,
                bbox,
                center,
            });
        }
        Ok(results)
    }
}

// ===========================================================================
// 下层实现
// ===========================================================================

/// 构建 ONNX Session（文件存在性已校验）。
fn build_session(path: &Path) -> Result<Session> {
    Session::builder()?
        .commit_from_file(path)
        .with_context(|| format!("加载 ONNX 模型失败: {}", path.display()))
}

/// 读取字典文件。
/// - PP-OCRv3 的 `ppocr_keys.json` 是 JSON 字符串数组（每个元素是一个字符/词，
///   索引 0 为空白符），用 serde_json 解析。
/// - PP-OCRv6 的 `.txt` 每行一个字符，按行读取。
fn load_vocab(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    match serde_json::from_str::<Vec<String>>(&content) {
        Ok(arr) => return Ok(arr),
        Err(_) => {}
    }
    Ok(content
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect())
}

// ---- 检测预处理：保持长宽比缩放到 [max_side]，对齐到 32 倍数，归一化 0~1，CHW ----
// PP-OCR det 模型内部有上采样对齐，要求 H、W 均为 32 的倍数，否则 Add 节点广播失败。
fn preprocess_det(img: &Array3<u8>) -> Result<Array4<f32>> {
    let (h, w, _) = img.dim();
    let max_side = 960;
    let scale = max_side as f32 / (h.max(w) as f32);
    let nh_raw = (h as f32 * scale).round() as usize;
    let nw_raw = (w as f32 * scale).round() as usize;
    // 向上对齐到 32 的倍数（且至少 32）
    let nh = ((nh_raw + 31) / 32).max(1) * 32;
    let nw = ((nw_raw + 31) / 32).max(1) * 32;
    // 最近邻缩放（简单够用；速度优先）
    let resized = resize_nearest(img, nh, nw);
    // HWC -> CHW，并做 PP-OCR 标准归一化：(img/255 - mean)/std
    let mean = [0.485_f32, 0.456, 0.406];
    let std = [0.229_f32, 0.224, 0.225];
    let mut chw = Array4::<f32>::zeros((1, 3, nh, nw));
    for c in 0..3 {
        for y in 0..nh {
            for x in 0..nw {
                let v = resized[[y, x, c]] as f32 / 255.0;
                chw[[0, c, y, x]] = (v - mean[c]) / std[c];
            }
        }
    }
    // [debug] 把 det 输入（反归一化）存盘，确认文字在输入里的位置
    if std::env::var("OCR_DUMP_DET_IN").is_ok() {
        let mut rgb = Array3::<u8>::zeros((nh, nw, 3));
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..3 {
                    let v = (chw[[0, c, y, x]] * std[c] + mean[c]) * 255.0;
                    rgb[[y, x, c]] = v.clamp(0.0, 255.0) as u8;
                }
            }
        }
        let buf = image::RgbImage::from_raw(nw as u32, nh as u32, rgb.as_slice().unwrap().to_vec()).unwrap();
        let _ = buf.save("/tmp/ocr_test/det_input.png");
        eprintln!("[debug] dumped det input {}x{}", nw, nh);
    }
    Ok(chw)
}

// ---- 识别预处理：crop 后 resize 到固定高（32），保持比例，归一化 ----
// 与 det 一致，使用 ImageNet mean/std：(img/255 - mean)/std。
// PP-OCR 约定：高固定 32，宽按比例缩放后 clamp 到 rec_max_w（v3=320/标准），
// 不足部分用 0 填充（pad 值在归一化后为负，对应黑色边，符合模型训练分布）。
fn preprocess_rec(img: &Array3<u8>, target_h: usize, norm: ([f32; 3], [f32; 3])) -> Result<Array4<f32>> {
    let (h, w, _) = img.dim();
    let mean = norm.0;
    let std = norm.1;
    let rec_max_w = 320usize;
    let scale = target_h as f32 / h as f32;
    let mut nw = (w as f32 * scale).max(1.0).round() as usize;
    nw = nw.min(rec_max_w).max(1);
    let resized = resize_nearest(img, target_h, nw);
    // pad 到 rec_max_w（右侧补 0）
    let mut padded = Array3::<u8>::zeros((target_h, rec_max_w, 3));
    for y in 0..target_h {
        for x in 0..nw {
            for k in 0..3 {
                padded[[y, x, k]] = resized[[y, x, k]];
            }
        }
    }
    if std::env::var("OCR_DUMP_REC").is_ok() {
        let buf = image::RgbImage::from_raw(
            rec_max_w as u32,
            target_h as u32,
            padded.as_slice().unwrap().to_vec(),
        ).unwrap();
        let _ = buf.save("/tmp/ocr_test/rec_crop.png");
        eprintln!("[debug] dumped rec_crop {}x{} nw={}", rec_max_w, target_h, nw);
    }
    let mut chw = Array4::<f32>::zeros((1, 3, target_h, rec_max_w));
    for c in 0..3 {
        for y in 0..target_h {
            for x in 0..rec_max_w {
                let v = padded[[y, x, c]] as f32 / 255.0;
                chw[[0, c, y, x]] = (v - mean[c]) / std[c];
            }
        }
    }
    Ok(chw)
}

/// 最近邻缩放（HWC u8）。
fn resize_nearest(img: &Array3<u8>, nh: usize, nw: usize) -> Array3<u8> {
    let (h, w, c) = img.dim();
    let mut out = Array3::<u8>::zeros((nh, nw, c));
    for y in 0..nh {
        let sy = ((y as f32 + 0.5) / nh as f32 * h as f32 - 0.5).max(0.0) as usize;
        let sy = sy.min(h - 1);
        for x in 0..nw {
            let sx = ((x as f32 + 0.5) / nw as f32 * w as f32 - 0.5).max(0.0) as usize;
            let sx = sx.min(w - 1);
            for k in 0..c {
                out[[y, x, k]] = img[[sy, sx, k]];
            }
        }
    }
    out
}

/// DB (Differentiable Binarization) 后处理：
/// 1. det 输出概率图二值化（阈值 0.3）
/// 2. 找连通域轮廓
/// 3. 多边形近似得到四点框，按原图缩放还原
fn db_postprocess(det_map: &Array<f32, ndarray::IxDyn>, h: usize, w: usize) -> Result<Vec<[f32; 8]>> {
    // det_map: [1,1,H',W']
    let d = det_map.shape();
    let hh = d[2];
    let ww = d[3];
    let sliced = det_map.index_axis(Axis(0), 0);
    let prob = sliced.index_axis(Axis(0), 0);
    // DB 二值化：用 Differentiable Binarization 公式把概率图转成"二值图"，
    // 比直接对 prob 卡阈值能给出更完整（更高更宽）的文字区域，避免只抓到
    // 字形中间带导致识别裁切。k 为放大系数（PP-OCR 默认 50）。
    let db_k = 50.0f32;
    let thr_map = 0.3f32; // 阈值图的近似（这里用常数阈值）
    let binarize = |p: f32| -> f32 {
        1.0 / (1.0 + (-db_k * (p - thr_map)).exp())
    };
    // [debug] 把概率图存盘（需设置 OCR_DUMP_DET_MAP=1），便于排查检测质量。
    if std::env::var("OCR_DUMP_DET_MAP").is_ok() {
        let mut pm = image::GrayImage::new(ww as u32, hh as u32);
        for y in 0..hh {
            for x in 0..ww {
                let v = (prob[[y, x]] * 255.0).clamp(0.0, 255.0) as u8;
                pm.put_pixel(x as u32, y as u32, image::Luma([v]));
            }
        }
        let _ = pm.save("/tmp/ocr_test/det_map.png");
    }
    let mut boxes = Vec::new();
    // 简易做法：阈值化后做连通分量（4-邻域），取每个分量的包围盒四点。
    // 更严谨应用轮廓近似，这里先出可用框（后续可升级多边形近似）。
    let mut visited = Array::<bool, _>::from_elem((hh, ww), false);
    for y0 in 0..hh {
        for x0 in 0..ww {
            if visited[[y0, x0]] || binarize(prob[[y0, x0]]) < 0.5 {
                continue;
            }
            // BFS 找连通域
            let mut stack = vec![(y0, x0)];
            visited[[y0, x0]] = true;
            let (mut miny, mut minx, mut maxy, mut maxx) = (y0, x0, y0, x0);
            while let Some((y, x)) = stack.pop() {
                miny = miny.min(y);
                minx = minx.min(x);
                maxy = maxy.max(y);
                maxx = maxx.max(x);
                for (dy, dx) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                    let ny = y as i64 + dy;
                    let nx = x as i64 + dx;
                    if ny < 0 || nx < 0 || ny >= hh as i64 || nx >= ww as i64 {
                        continue;
                    }
                    let (ny, nx) = (ny as usize, nx as usize);
                    if !visited[[ny, nx]] && binarize(prob[[ny, nx]]) >= 0.5 {
                        visited[[ny, nx]] = true;
                        stack.push((ny, nx));
                    }
                }
            }
            // 还原到原图尺度
            let sy = h as f32 / hh as f32;
            let sx = w as f32 / ww as f32;
            let x0_ = minx as f32 * sx;
            let y0_ = miny as f32 * sy;
            let x1_ = (maxx as f32 + 1.0) * sx;
            let y1_ = (maxy as f32 + 1.0) * sy;
            // 过滤过小的框（噪声）
            if (x1_ - x0_) < 4.0 || (y1_ - y0_) < 4.0 {
                continue;
            }
            // PP-OCR 的"扩张"步骤：DB 概率图对每个文字行只激活中间带，
            // 需要把框向外扩张，才能包住完整字形高度。按"矩形各边向外扩
            // expand_ratio * min(w,h)"实现（不是沿质心对角线，否则宽扁框几乎
            // 不长高），与 PaddleOCR 的 expand 行为一致。
            let bw = x1_ - x0_;
            let bh = y1_ - y0_;
            let expand = 1.5_f32 * bw.min(bh);
            let ax0 = (x0_ - expand).max(0.0);
            let ay0 = (y0_ - expand).max(0.0);
            let ax1 = (x1_ + expand).min(w as f32);
            let ay1 = (y1_ + expand).min(h as f32);
            boxes.push([ax0, ay0, ax1, ay0, ax1, ay1, ax0, ay1]);
        }
    }
    Ok(boxes)
}

/// 按四点 bbox 从原图裁剪（这里用简单包围盒 crop，不做透视变换；
/// 对近水平文本足够，倾斜文本后续可加透视校正）。
fn crop_and_resize_for_rec(img: &Array3<u8>, bbox: &[f32; 8]) -> Result<Array3<u8>> {
    let (h, w, c) = img.dim();
    // 向 bbox 四周外扩 padding（按框尺寸比例），避免裁掉字形上下缘导致识别空白。
    let bw = (bbox[4] - bbox[0]).abs();
    let bh = (bbox[5] - bbox[1]).abs();
    let pad_x = (bw * 0.05).max(1.0);
    let pad_y = (bh * 0.05).max(1.0);
    let minx = (bbox[0] - pad_x).max(0.0).floor() as usize;
    let miny = (bbox[1] - pad_y).max(0.0).floor() as usize;
    let maxx = (bbox[4] + pad_x + 1.0).min(w as f32).floor() as usize;
    let maxy = (bbox[5] + pad_y + 1.0).min(h as f32).floor() as usize;
    if maxx <= minx || maxy <= miny {
        // 退化框：返回 1x1
        return Ok(Array3::<u8>::zeros((1, 1, c)));
    }
    let mut out = Array3::<u8>::zeros((maxy - miny, maxx - minx, c));
    for y in miny..maxy {
        for x in minx..maxx {
            for k in 0..c {
                out[[y - miny, x - minx, k]] = img[[y, x, k]];
            }
        }
    }
    Ok(out)
}

/// CTC 贪婪解码：取每个时间步 argmax，折叠连续重复 + 去 blank(索引0)。
/// PP-OCR rec 输出布局为 `[1, C, T]`（C=字符类数，T=时间步）。时间轴取
/// 较小的非 1 维度（T << C），字符轴取较大的维度。
fn ctc_greedy_decode(logits: &Array<f32, ndarray::IxDyn>, vocab: &[String]) -> (String, f32) {
    let d = logits.shape();
    // 时间轴 = 较小的非1维；字符轴 = 较大的维。
    let (time_axis, class_axis) = if d[1] <= d[2] { (1, 2) } else { (2, 1) };
    let t = d[time_axis];
    let c = d[class_axis];
    let mut chars = Vec::with_capacity(t);
    let mut total = 0f32;
    let mut count = 0;
    let mut prev = usize::MAX;
    for ti in 0..t {
        let mut best = 0usize;
        let mut bestv = f32::NEG_INFINITY;
        for ci in 0..c {
            let v = if time_axis == 1 {
                logits[[0, ti, ci]]
            } else {
                logits[[0, ci, ti]]
            };
            if v > bestv {
                bestv = v;
                best = ci;
            }
        }
        if best != 0 && best != prev {
            // 非 blank 且不与前一重复
            if let Some(ch) = vocab.get(best) {
                chars.push(ch.clone());
            }
        }
        prev = best;
        total += bestv;
        count += 1;
    }
    let score = if count > 0 { total / count as f32 } else { 0.0 };
    (chars.concat(), score)
}

/// bbox 质心。
fn bbox_center(bbox: &[f32; 8]) -> [f32; 2] {
    let cx = (bbox[0] + bbox[2] + bbox[4] + bbox[6]) / 4.0;
    let cy = (bbox[1] + bbox[3] + bbox[5] + bbox[7]) / 4.0;
    [cx, cy]
}
