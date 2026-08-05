//! 过滤判断「有字幕」：FilterTransformedImage / AnalizeImageForSubPresence。
//!
//! 复刻 VideoSubFinder：找文字块连通域（CMyClosedFigure），按尺寸/密度过滤。
//! 策略：用 OpenCV `findContours` 替代手写连通域，保留过滤规则。

use ndarray::Array3;

/// 判断帧是否有字幕（AnalizeImageForSubPresence → FilterTransformedImage）。
/// `frame` 为已二值化的文字图；返回是否有字幕。
pub fn has_subtitle(_frame: &Array3<u8>, _params: &super::params::Params) -> bool {
    todo!("复刻 FilterTransformedImage，用 OpenCV findContours")
}
