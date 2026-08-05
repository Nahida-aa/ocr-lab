//! 图像工具：不含业务语义的纯图像转换/处理辅助。
//!
//! 与具体业务（OCR、颜色分析、操作注入）解耦——谁需要「抓来的 RGBA 图转 RGB」、
//! 「降采样」「裁剪」这类通用操作，就依赖本 crate，而不是把这些helper散落在
//! `ocr-agent` / `screen-operator` 里。
//!
//! 当前提供：
//! - [`rgba_to_rgb`]：抓图后端（capturer）给的 `RgbaImage` → `RgbImage`（丢 alpha）。

use image::{RgbImage, RgbaImage};

/// 把 capturer 抓来的 `RgbaImage` 转成 `RgbImage`（丢弃 alpha）。
///
/// OCR / 颜色分析 / 计算缩放都不需要 alpha 通道，转掉省内存也省下游分支。
pub fn rgba_to_rgb(img: &RgbaImage) -> RgbImage {
    image::DynamicImage::ImageRgba8(img.clone()).to_rgb8()
}
