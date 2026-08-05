//! 逐帧预处理：相邻帧交集（去噪）+ 水平投影检测文字（AnalyseImage）。
//!
//! 复刻 VideoSubFinder 的 `IntersectTwoImages`（相邻帧逐像素交集去单帧闪烁）与
//! `AnalyseImage`（按 `segh` 水平条带投影、`tp` 文字占比、`mtpl` 最小长度判断有无文字）。

use ndarray::Array3;

/// 相邻帧交集：逐像素取两帧的公共文字像素，去单帧闪烁/噪声。
/// `a` 与 `b` 均为 BGR H×W×3，输出与 `a` 同尺寸。
pub fn intersect_two_images(_a: &Array3<u8>, _b: &Array3<u8>) -> Array3<u8> {
    todo!("复刻 IntersectTwoImages")
}

/// 分析一帧是否有文字行（AnalyseImage 的水平条带投影）。
/// 返回是否检测到文字。
pub fn analyse_image(_frame: &Array3<u8>, _params: &super::params::Params) -> bool {
    todo!("复刻 AnalyseImage")
}
