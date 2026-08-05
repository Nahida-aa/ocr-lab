//! 检测 / 识别 / 方向分类 三分支的输入预处理。
//!
//! 对齐 subtitle-rust（即 Python rapidocr）的做法：
//! - Det: 把**短边**缩放到 736，向下取整到 32 网格（同 Python/CPP）。
//! - Cls: resize 到 48×192，归一化 `(x/255 - 0.5)/0.5`。
//! - Rec: 固定高度 48，保持纵横比，`img_w = int(48 * ratio)`（**不封顶**，
//!   这样宽行不会被压扁、不会掉中段字符）。归一化同样 `(x/255 - 0.5)/0.5`。
//!
//! 与 subtitle-rust 的区别：我们不做 `--subtitle-only` 的底部裁剪（那是字幕
//! 特化的），但 det 的短边缩放逻辑一致。

use ndarray::{Array3, Array4};
use std::path::Path;

/// Det 输入短边目标尺寸（对齐 Python rapidocr 的 `det_limit_side_len=736`）。
pub const DET_LIMIT_SIDE: usize = 736;
/// Cls 输入尺寸。
pub const CLS_W: usize = 192;
pub const CLS_H: usize = 48;
/// Rec 输入高度（PP-OCR v3 / v6 的 rec 均吃 48，v3 之前误用 32 是掉字根因）。
pub const REC_H: usize = 48;

/// 识别分支归一化参数 `(mean, std)`。
/// PP-OCR 训练时用的就是 `(x/255 - 0.5)/0.5`，与 ImageNet 统计不同；v3 之前
/// 误用了 ImageNet 归一化，模型其实是同一套权重，故统一改回 (0.5,0.5)。
pub const REC_NORM: ([f32; 3], [f32; 3]) = ([0.5, 0.5, 0.5], [0.5, 0.5, 0.5]);

/// 检测预处理：保持长宽比缩放到短边=736，对齐到 32 倍数，归一化 (x/255-mean)/std。
///
/// 返回 `(chw_tensor, resized_h, resized_w)`，其中 tensor 为 `[1,3,H,W]`。
pub fn preprocess_det(img: &Array3<u8>) -> (Array4<f32>, usize, usize) {
    let (h, w, _) = img.dim();
    let (nh, nw) = det_target_size(h, w);
    // det 用 image crate 的 Triangle 双线性缩放。曾尝试换成 opencv INTER_LINEAR
    // （与 cpp 的 cv::resize 严格一致）以对齐热力图，但实测让 rust 的聚合指标
    // 反而偏离 cpp（CER(norm) 0.54%→0.89%、spurious 2→5）——因为 rust 的 det
    // 后处理几何（轮廓轴对齐包围盒而非 minAreaRect + 不同 unclip）与 cpp 不同，
    // 仅靠对齐缩放核无法收敛，故回退到 Triangle。真正的残余差异在几何，不在核。
    let resized = resize_bilinear(img, nh, nw);
    let mean = [0.485_f32, 0.456, 0.406];
    let std = [0.229_f32, 0.224, 0.225];
    let chw = normalize_chw(&resized, &mean, &std);
    (chw, nh, nw)
}

/// 计算 det 目标尺寸：对齐 cpp / PP-OCR 官方 Python 的缩放约定。
///
/// cpp（`preprocessDet`，`limit_type='min'`）：
///   if min(h, w) < DET_LIMIT_SIDE: ratio = DET_LIMIT_SIDE / min(h, w)
///   else:                           ratio = 1.0  // 短边已 ≥736 则不缩放
///   newH/newW = round(dim * ratio / 32) * 32，且 ≥ 32。
///
/// 注意：ratio 基于**短边**算、对整个图统一缩放（不是只把短边固定为 736
/// 再单独算长边）。这与旧实现（永远 736*ratio + 向下取整）不同。两版 det 输入
/// 尺寸现在一致，使差异纯粹来自实现本身（缩放插值核、后处理几何等）。
pub fn det_target_size(h: usize, w: usize) -> (usize, usize) {
    let short = h.min(w);
    let ratio = if short < DET_LIMIT_SIDE {
        DET_LIMIT_SIDE as f32 / short as f32
    } else {
        1.0_f32
    };
    // 对齐 cpp 顺序：先按 ratio 截断缩放（int 截断），再 round(dim/32)*32。
    let nh = ((h as f32 * ratio) as usize / 32) as f32;
    let nw = ((w as f32 * ratio) as usize / 32) as f32;
    let nh = (nh.round() * 32.0) as usize;
    let nw = (nw.round() * 32.0) as usize;
    (nh.max(32), nw.max(32))
}

/// 识别预处理：crop 已经裁出文字块，这里把它 resize 到 `[REC_H, img_w]`，
/// `img_w = int(REC_H * (w/h))` 且**不做任何封顶**——这正是 subtitle-rust 与
/// 我之前代码（clamp 到 320）的关键差异：封顶会把宽行压扁、挤掉中段字符。
///
/// 返回 `(chw_tensor, rec_w)`。tensor 形状 `[1,3,REC_H,rec_w]`。
pub fn preprocess_rec(img: &Array3<u8>) -> (Array4<f32>, usize) {
    let (h, w, _) = img.dim();
    let ratio = w as f32 / h as f32;
    // 对齐 cpp preprocessRec：imgW = int(48*ratio)（floor），resizedW = imgW（非整数时）。
    // 旧实现用 round()，与 cpp 的 floor 在 48*ratio 非整数时差 1px，足以翻转形近字（白/百）。
    let img_w = ((REC_H as f32 * ratio) as usize).max(1);
    let resized_w = if (REC_H as f32 * ratio).ceil() > img_w as f32 {
        img_w
    } else {
        (REC_H as f32 * ratio).ceil() as usize
    };
    let resized_w = resized_w.max(1);
    let resized = resize_bilinear(img, REC_H, resized_w);
    let (mean, std) = REC_NORM;
    let chw = normalize_chw(&resized, &mean, &std);
    // 对齐 cpp：tensor 宽 = imgW（可能 > resized_w，右侧补零）。
    if img_w > resized_w {
        let mut padded = Array4::<f32>::zeros((1, 3, REC_H, img_w));
        for ci in 0..3 {
            for y in 0..REC_H {
                for x in 0..resized_w {
                    padded[[0, ci, y, x]] = chw[[0, ci, y, x]];
                }
            }
        }
        return (padded, img_w);
    }
    (chw, img_w)
}

/// 方向分类预处理：resize 到 `[CLS_H, CLS_W]`，归一化 (x/255-0.5)/0.5。
pub fn preprocess_cls(img: &Array3<u8>) -> Array4<f32> {
    let resized = resize_bilinear(img, CLS_H, CLS_W);
    normalize_chw(&resized, &[0.5, 0.5, 0.5], &[0.5, 0.5, 0.5])
}

/// HWC u8 -> CHW f32 并归一化 `(v/255 - mean)/std`。
fn normalize_chw(img: &Array3<u8>, mean: &[f32; 3], std: &[f32; 3]) -> Array4<f32> {
    let (h, w, c) = img.dim();
    let mut chw = Array4::<f32>::zeros((1, c, h, w));
    for ci in 0..c {
        for y in 0..h {
            for x in 0..w {
                let v = img[[y, x, ci]] as f32 / 255.0;
                chw[[0, ci, y, x]] = (v - mean[ci]) / std[ci];
            }
        }
    }
    chw
}

/// 双线性缩放（HWC u8），基于 `image` crate 的 `imageops::resize`（Triangle）。
/// 比最近邻保留更多笔画细节，识别更稳。
pub fn resize_bilinear(img: &Array3<u8>, nh: usize, nw: usize) -> Array3<u8> {
    let (h, w, c) = img.dim();
    let mut buf: Vec<u8> = Vec::with_capacity(h * w * c);
    for y in 0..h {
        for x in 0..w {
            for k in 0..c {
                buf.push(img[[y, x, k]]);
            }
        }
    }
    let src = image::RgbImage::from_raw(w as u32, h as u32, buf).expect("构造源图失败");
    let dst = image::imageops::resize(&src, nw as u32, nh as u32, image::imageops::FilterType::Triangle);
    let mut out = Array3::<u8>::zeros((nh, nw, c));
    for y in 0..nh {
        for x in 0..nw {
            let p = dst.get_pixel(x as u32, y as u32);
            for k in 0..c {
                out[[y, x, k]] = p.0[k];
            }
        }
    }
    out
}

/// 最近邻缩放（HWC u8），用于 det 这种只需粗略对齐的场合（速度优先）。
#[allow(dead_code)]
pub fn resize_nearest(img: &Array3<u8>, nh: usize, nw: usize) -> Array3<u8> {
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

/// 调试用：把某张图存盘（需显式调用，main/test 里按需启用）。
#[allow(dead_code)]
pub fn dump_png(path: &Path, img: &Array3<u8>) {
    let (h, w, _c) = img.dim();
    let buf = img.as_slice().unwrap().to_vec();
    if let Some(rgb) = image::RgbImage::from_raw(w as u32, h as u32, buf) {
        let _ = rgb.save(path);
    }
}
