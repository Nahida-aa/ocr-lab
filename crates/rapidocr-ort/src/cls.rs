//! 方向分类（cls）分支：判断文字块是否需旋转 180°。
//!
//! PP-OCR 的 cls 模型输入 48×192，输出 2 类：0=正向，1=倒置。若 `out[1] >
//! out[0]` 说明文字是倒的，rec 前需 `rotate_180`。

use ndarray::Array3;

/// 是否需要旋转 180°（cls 输出类 1 概率 > 类 0）。
pub fn need_rotate_180(cls_out: &[f32]) -> bool {
    cls_out.len() >= 2 && cls_out[1] > cls_out[0]
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
