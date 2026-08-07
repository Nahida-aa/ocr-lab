//! # subtitle-ocr
//!
//! 字幕 OCR 专用层，构建在 [`rapidocr_ort::OcrEngine`]（PP-OCR det/rec/cls）之上。
//!
//! 本包不做模型推理，只做「字幕场景」的专属逻辑，与 cpp 实现
//! （`packages/subtitle-ocr-cpp/ocr_pipeline.cpp`）对齐，便于 bench 公平对比：
//!
//! - **bottom_only ROI**：只送画面底部 40% 给引擎，提速（cpp 默认开启）。
//! - **subtitle_only y 过滤**：仅保留 y 中心落在底部比例区间的字幕框。
//! - **NMS 去重**：剔除被大框高度覆盖的重叠框（cpp `--no-nms` 可关）。
//! - **多帧合并 + 计时**：相邻帧同文本合并成带 `start/end` 的段（LocalDub
//!   `mergeFrames` 风格）。
//!
//! 计时口径：模型只加载一次，推理耗时由调用方自行测量（在 `ocr_image` 调用前后
//! `Instant::now()` 即可）。rapidocr-ort 的 `detect` 把 det/rec 合成一次调用，无法
//! 单独计时，故本包不内置计时。这些耗时是旁路观测数据，不进入 JSON 输出。

use anyhow::Result;
use ndarray::{Array3, s};
use rapidocr_ort::{ModelProfile, OcrEngine};

// ===========================================================================
// 选项与结果类型
// ===========================================================================

/// 字幕 OCR 的行为开关（对齐 cpp 的 CLI 参数）。
#[derive(Clone, Debug)]
pub struct OcrOptions {
    /// 只裁底部 40% 送 OCR（cpp 默认 true）。
    pub bottom_only: bool,
    /// 仅保留 y 中心在画面底部比例区间的字幕框（cpp `--subtitle-only`）。
    pub subtitle_only: bool,
    /// 重叠框 NMS 去重（cpp 默认 true，`--no-nms` 关闭）。
    pub use_nms: bool,
    /// 识别置信度下限（cpp `text_score`，默认 0.5）。
    pub text_score: f32,
    /// 是否用 cpp 同款的透视矫正裁剪（warpPerspective）替代轴对齐包围盒。
    /// 配合 det 几何 minAreaRect 一起用（两者耦合）；默认 false。
    pub use_warp_crop: bool,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            bottom_only: true,
            subtitle_only: false,
            use_nms: true,
            text_score: 0.5,
            use_warp_crop: false,
        }
    }
}

/// 单个文字识别区域（`rapidocr_ort::OcrBoxResult` 的 re-export）。
///
/// 原先是自定义 `FrameLine` 结构体，字段几乎与 rapidocr-ort 的 `OcrResult`
/// 相同（text/confidence/box_），仅多了 `y_center`（= `center[1]`）。后改为
/// 类型别名，并统一命名为 `OcrBoxResult`（表示"一个识别区域/文本框"）。
/// ⚠️ 坐标语义：`ocr_image` 返回前会把 box/center 的 y 加回 `y_offset`，
/// 还原成原图坐标。
pub use rapidocr_ort::OcrBoxResult;

/// 单图聚合结果（可携带单时刻时间戳）。
///
/// 把一张图里识别出的多框聚合成一条文本 + 值域 + 明细。`timestamp_ms` 为
/// 单时刻（毫秒）：默认 `0` 表示「无时间」；当上游按文件名（`ms` / `ms_ms`）
/// 或帧序号代入时携带该图对应时刻。`ms_ms` 文件名会让同一张图被识别一次、
/// 产出两个 `FrameResult`（各自带 start/end 时刻、内容相同）。
///
/// 本库仍只吃一张图、不知道图片整体来源结构；把多个 `FrameResult` 合并成
/// 带时间轴的字幕段由独立合并层 `subtitle-ocr-merge` 负责。
#[derive(Clone, Debug)]
pub struct FrameResult {
    /// 该图识别文本（多行按出现顺序拼接，用空格分隔）。
    pub text: String,
    /// 该图聚合置信度：各框 `text_confidence` 的均值（对齐下游 TS `aggregate_boxes`，
    /// 多框取平均而非最大）。
    pub confidence: f64,
    /// 该图所有识别区域明细（每行文本/框/score，含坐标还原）。
    pub boxes: Vec<OcrBoxResult>,
    /// 横向值域 `[min_x, max_x]`（像素坐标），无字幕时为 `[0,0]`。
    pub x_range: [f32; 2],
    /// 纵向值域 `[min_y, max_y]`（像素坐标），无字幕时为 `[0,0]`。
    pub y_range: [f32; 2],
    /// 该图对应时刻（毫秒）。`0` 表示无时间（如单图 `<image>` 调用、或
    /// `aggregate_boxes` 纯聚合后尚未赋值）。
    ///
    /// 为何不交给 `aggregate_boxes` 填：同一张图可能对应多个时刻（`ms_ms` 文件名
    /// 的 start/end），为避免对同一 boxes 重复聚合，聚合函数保持单参数、只产出
    /// 无时间结果；调用方聚合一次后，按各时刻 `clone` 并覆写本字段。时间来源由
    /// 上游按文件名 `ms`/`ms_ms` 解析、帧序号或视频 PTS 提供。
    pub timestamp_ms: u64,
}

// ===========================================================================
// 引擎封装
// ===========================================================================

/// 字幕 OCR 引擎：持有 [`OcrEngine`] 与行为选项。
pub struct SubtitleOcr {
    engine: OcrEngine,
    opts: OcrOptions,
}

impl SubtitleOcr {
    /// 按模型套件构建（模型目录默认仓库根 `models/rapidocr`）。
    pub fn from_profile(
        profile: ModelProfile,
        model_dir: &std::path::Path,
        opts: OcrOptions,
    ) -> Result<Self> {
        let engine =
            OcrEngine::from_profile(profile, model_dir)?.with_warp_crop(opts.use_warp_crop);
        Ok(Self { engine, opts })
    }

    /// 对一帧 RGB 图像（H×W×3，0-255 u8）做字幕 OCR，返回排序后的识别行。
    ///
    /// 流程对齐 cpp `runOcr`：bottom_only ROI → subtitle_only y 过滤 → NMS。
    pub fn ocr_image(&mut self, rgb: &Array3<u8>) -> Result<Vec<OcrBoxResult>> {
        let (h, _, _) = rgb.dim();
        let h = h as i64;

        // ---- 1. bottom_only：裁底部 40% 作为 ROI ----
        let y_offset = if self.opts.bottom_only {
            ((h as f32) * 0.6) as i64
        } else {
            0
        };
        let roi: Array3<u8> = if y_offset > 0 {
            rgb.slice(s![y_offset as usize.., .., ..]).to_owned()
        } else {
            rgb.clone()
        };

        // ---- 2. 引擎推理（det + rec + cls）----
        let results: Vec<OcrBoxResult> = self.engine.detect(&roi)?;

        // ---- 3. 后处理：还原坐标 / y 过滤 / NMS / trim / 排序 ----
        let mut lines: Vec<OcrBoxResult> = results
            .into_iter()
            .map(|mut r| {
                // ROI 坐标还原回原图：box 每点 y 与 center.y 都加 y_offset。
                if y_offset > 0 {
                    for p in &mut r.box_ {
                        p[1] += y_offset as f32;
                    }
                    r.center[1] += y_offset as f32;
                }
                r.text = r.text.trim().to_string();
                r
            })
            .filter(|l| {
                // subtitle_only：y 中心须落在画面底部 [0.85, 0.99]（cpp 比值口径）。
                if self.opts.subtitle_only {
                    let ratio = l.center[1] / (h as f32);
                    if !(0.85..=0.99).contains(&ratio) {
                        return false;
                    }
                }
                !l.text.is_empty() && l.text_confidence >= self.opts.text_score
            })
            .collect();

        if self.opts.use_nms && lines.len() > 1 {
            lines = nms(lines);
        }

        // 排序：先按 y 中心，差 ≤20px 再按 x 中心（cpp 的 TL/BR 排序等价）。
        lines.sort_by(|a, b| {
            let ya = a.center[1];
            let yb = b.center[1];
            if (ya - yb).abs() > 20.0 {
                ya.partial_cmp(&yb).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                let xa = a.box_[0][0];
                let xb = b.box_[0][0];
                xa.partial_cmp(&xb).unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        Ok(lines)
    }
}

/// 把一图识别出的多框聚合成单图结果（纯感知后处理，`timestamp_ms` 置 0）。
///
/// 过滤 / 坐标还原 / NMS / 排序已在 [`SubtitleOcr::ocr_image`] 完成，这里只做
/// 「多行拼接成一条文本 + 各框置信度取均值 + 算几何值域」。
///
/// `timestamp_ms` 故意不在此处填入（置 0），原因：同一张图可能对应多个时刻
/// （文件名 `ms_ms` 时间区间图片，start/end 两个时刻、内容相同）。保持单参数、
/// 不接 timestamp，调用方就能**先聚合一次**得到无时间的 [`FrameResult`]，再按
/// `entry.times` 展开——对每个时刻 `clone` 后仅改 `timestamp_ms`，避免对同一
/// boxes 重复聚合。携带时间的场景由调用方在 [`FrameResult`] 上赋值（如按文件名
/// `ms`/`ms_ms` 解析、或帧序号 / 视频 PTS）。
pub fn aggregate_boxes(lines: &[OcrBoxResult]) -> FrameResult {
    let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    let text = text.join(" ");
    let confidence = if lines.is_empty() {
        0.0
    } else {
        lines.iter().map(|l| l.text_confidence as f64).sum::<f64>() / lines.len() as f64
    };
    // 聚合所有行的四点坐标，取 x / y 值域（无字幕 → [0,0]）。
    let mut x_range = [f32::INFINITY, f32::NEG_INFINITY];
    let mut y_range = [f32::INFINITY, f32::NEG_INFINITY];
    for l in lines {
        for p in &l.box_ {
            x_range[0] = x_range[0].min(p[0]);
            x_range[1] = x_range[1].max(p[0]);
            y_range[0] = y_range[0].min(p[1]);
            y_range[1] = y_range[1].max(p[1]);
        }
    }
    let (x_range, y_range) = if lines.is_empty() {
        ([0.0, 0.0], [0.0, 0.0])
    } else {
        (x_range, y_range)
    };
    FrameResult {
        text,
        confidence,
        boxes: lines.to_vec(),
        x_range,
        y_range,
        timestamp_ms: 0,
    }
}

// ===========================================================================
// NMS（复刻 cpp runOcr 的重叠框过滤）
// ===========================================================================

/// 按面积降序，剔除被已保留大框覆盖超过 70% 的小框（IoU 口径）。
fn nms(mut lines: Vec<OcrBoxResult>) -> Vec<OcrBoxResult> {
    // 计算外接框。
    struct B {
        idx: usize,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        area: f32,
    }
    let mut boxes: Vec<B> = lines
        .iter()
        .enumerate()
        .map(|(idx, l)| {
            let (mut x0, mut y0, mut x1, mut y1) = (
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            );
            for p in &l.box_ {
                x0 = x0.min(p[0]);
                x1 = x1.max(p[0]);
                y0 = y0.min(p[1]);
                y1 = y1.max(p[1]);
            }
            let area = (x1 - x0).max(1.0) * (y1 - y0).max(1.0);
            B {
                idx,
                x0,
                y0,
                x1,
                y1,
                area,
            }
        })
        .collect();
    // 面积大的优先保留。
    boxes.sort_by(|a, b| {
        b.area
            .partial_cmp(&a.area)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut keep = vec![true; lines.len()];
    for i in 0..boxes.len() {
        if !keep[boxes[i].idx] {
            continue;
        }
        let a = &boxes[i];
        for j in (i + 1)..boxes.len() {
            if !keep[boxes[j].idx] {
                continue;
            }
            let b = &boxes[j];
            let ix0 = a.x0.max(b.x0);
            let iy0 = a.y0.max(b.y0);
            let ix1 = a.x1.min(b.x1);
            let iy1 = a.y1.min(b.y1);
            if ix1 <= ix0 || iy1 <= iy0 {
                continue;
            }
            let inter = (ix1 - ix0) * (iy1 - iy0);
            // 小框 b 被大框 a 覆盖比例。
            if inter / b.area > 0.7 {
                keep[boxes[j].idx] = false;
            }
        }
    }
    let mut out: Vec<OcrBoxResult> = keep
        .iter()
        .zip(lines.drain(..))
        .filter(|(k, _)| **k)
        .map(|(_, l)| l)
        .collect();
    // 维持原顺序（按 y 排序在 ocr_image 末尾统一做；此处仅 NMS 过滤，先按输入序）。
    out.sort_by_key(|l| (l.center[1] * 1000.0) as i64);
    out
}

// ===========================================================================
// 白盒测试（纯函数，无需 ONNX 模型）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nms_removes_contained_small_box() {
        // 大框包含小框（覆盖 >70%）→ 小框被剔除。
        let big = OcrBoxResult {
            text: "A".into(),
            text_confidence: 0.9,
            box_: [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]],
            box_confidence: 0.9,
            x_range: [0.0, 100.0],
            y_range: [0.0, 100.0],
            center: [50.0, 50.0],
        };
        let small = OcrBoxResult {
            text: "B".into(),
            text_confidence: 0.8,
            box_: [[10.0, 10.0], [20.0, 10.0], [20.0, 20.0], [10.0, 20.0]],
            box_confidence: 0.8,
            x_range: [10.0, 20.0],
            y_range: [10.0, 20.0],
            center: [15.0, 15.0],
        };
        let out = nms(vec![big.clone(), small]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "A");
    }

    #[test]
    fn nms_keeps_disjoint_boxes() {
        let a = OcrBoxResult {
            text: "A".into(),
            text_confidence: 0.9,
            box_: [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            box_confidence: 0.9,
            x_range: [0.0, 10.0],
            y_range: [0.0, 10.0],
            center: [5.0, 5.0],
        };
        let b = OcrBoxResult {
            text: "B".into(),
            text_confidence: 0.9,
            box_: [
                [100.0, 100.0],
                [110.0, 100.0],
                [110.0, 110.0],
                [100.0, 110.0],
            ],
            box_confidence: 0.9,
            x_range: [100.0, 110.0],
            y_range: [100.0, 110.0],
            center: [105.0, 105.0],
        };
        let out = nms(vec![a, b]);
        assert_eq!(out.len(), 2);
    }
}
