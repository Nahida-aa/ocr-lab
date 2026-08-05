//! 识别流水线里的「裁剪 → 分类(可选) → 识别」步骤。
//!
//! 与 subtitle-rust 的 `pipeline.rs::run_ocr_core` 对应，但做了简化：不做
//! `--subtitle-only` 的底部区域过滤、不做透视变换（仅包围盒 crop，对近水平
//! 文本足够）。cls 分支默认启用，但可关。

use ndarray::Array3;
use opencv::core::Point2f;
use ort::session::Session;

use crate::cls;
use crate::preprocess::preprocess_rec;
use crate::rec::ctc_greedy_decode;

/// 按四点 bbox 从原图裁剪（包围盒 crop，不做透视变换；对近水平文本足够）。
/// `polygon` 为四个顶点（顺时针），这里取其 x/y 的极值作为包围盒，向四周外扩
/// 5% 避免裁掉字形上下缘。对任意四边形（含旋转框）都取正确包围盒。
pub fn crop_for_rec(img: &Array3<u8>, polygon: &[Point2f; 4]) -> Array3<u8> {
    let (h, w, c) = img.dim();
    let minx_f = polygon.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let maxx_f = polygon
        .iter()
        .map(|p| p.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let miny_f = polygon.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let maxy_f = polygon
        .iter()
        .map(|p| p.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let bw = (maxx_f - minx_f).abs();
    let bh = (maxy_f - miny_f).abs();
    let pad_x = (bw * 0.05).max(1.0);
    let pad_y = (bh * 0.05).max(1.0);
    let minx = (minx_f - pad_x).max(0.0).floor() as usize;
    let miny = (miny_f - pad_y).max(0.0).floor() as usize;
    let maxx = (maxx_f + pad_x + 1.0).min(w as f32).floor() as usize;
    let maxy = (maxy_f + pad_y + 1.0).min(h as f32).floor() as usize;
    if maxx <= minx || maxy <= miny {
        return Array3::<u8>::zeros((1, 1, c));
    }
    let mut out = Array3::<u8>::zeros((maxy - miny, maxx - minx, c));
    for y in miny..maxy {
        for x in minx..maxx {
            for k in 0..c {
                out[[y - miny, x - minx, k]] = img[[y, x, k]];
            }
        }
    }
    out
}

/// 对一个裁剪块跑 rec，返回 `(文本, 置信度)`。
///
/// `rec` 为 rec Session；`rec_out_name` 为其输出节点名；`vocab` 为字典（索引 0
/// 为 blank）。`use_cls` 为真时先跑 cls（输出节点名 `cls_out_name`）判断是否需要旋转 180°。
pub fn recognize(
    img: &Array3<u8>,
    rec: &mut Session,
    rec_out_name: &str,
    cls: &mut Session,
    cls_out_name: &str,
    use_cls: bool,
    vocab: &[String],
) -> (String, f32) {
    // 可选方向分类
    let rec_crop = if use_cls {
        let cls_in = crate::preprocess::preprocess_cls(img);
        let cls_tensor = ort::value::Tensor::from_array(cls_in).expect("构造 cls 输入张量失败");
        let cls_out = cls
            .run(ort::inputs!["x" => cls_tensor])
            .expect("cls 推理失败");
        let cls_arr = cls_out[cls_out_name]
            .try_extract_array::<f32>()
            .unwrap()
            .to_owned();
        let cls_slice: Vec<f32> = cls_arr.as_slice().unwrap().to_vec();
        if cls::need_rotate_180(&cls_slice) {
            cls::rotate_180(img)
        } else {
            img.clone()
        }
    } else {
        img.clone()
    };

    let (rec_in, _rec_w) = preprocess_rec(&rec_crop);
    let rec_tensor = ort::value::Tensor::from_array(rec_in).expect("构造 rec 输入张量失败");
    let rec_out = rec
        .run(ort::inputs!["x" => rec_tensor])
        .expect("rec 推理失败");
    let logits = rec_out[rec_out_name]
        .try_extract_array::<f32>()
        .unwrap()
        .to_owned();
    // rec 输出形状 [1, T, C]（C=字符类数，T=时间步），直接取最后两维。
    let shape = logits.shape().to_vec();
    let flat = logits.as_slice().unwrap().to_vec();
    ctc_greedy_decode(&flat, &shape, vocab)
}
