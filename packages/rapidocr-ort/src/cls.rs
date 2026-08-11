//! 方向分类（cls）分支：判断文字块是否需旋转 180°。
//!
//! PP-OCR 的 cls 模型输入 48×192，输出 2 类：0=正向，1=倒置（概率）。
//! 对齐 Python RapidOCR：仅当 **类 1 概率 > cls_thresh(0.9)** 才旋转。
//! 不能用 `out[1] > out[0]`（即 0.5 阈值）——cls 对含噪声/边缘的 crop 极易
//! 给出 0.6~0.7 的弱倒置信号，按 0.5 阈值会误把正立字幕旋成倒置，导致 rec
//! 全面错识（实测 "不过" 被误旋后识别成 "RL"/"FL"）。

use ndarray::Array3;

/// 类 1（倒置）概率超过此阈值才判定需旋转 180°，对齐 Python 的 `cls_thresh`。
const CLS_THRESH: f32 = 0.9;

/// 是否需要旋转 180°（cls 输出类 1 概率 > `CLS_THRESH`）。
pub fn need_rotate_180(cls_out: &[f32]) -> bool {
    cls_out.len() >= 2 && cls_out[1] > CLS_THRESH
}

/// 旋转 180°（HWC u8）。
pub fn rotate_180(img: &Array3<u8>) -> Array3<u8> {
    let (h, w, c) = img.dim();
    let mut out = Array3::<u8>::zeros((h, w, c));
    for y in 0..h {
        for x in 0..w {
            for k in 0..c {
                out[[h - 1 - y, w - 1 - x, k]] = img[[y, x, k]];
            }
        }
    }
    out
}
