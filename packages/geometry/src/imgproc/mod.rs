//! 纯 Rust 图像像素算子（SIMD 优化），替代/降低对 OpenCV 绑定在像素级处理上的依赖。
//!
//! 按算子类型拆成子模块：
//! - [`resize`] —— HWC u8 双线性缩放，**对齐 OpenCV `cv::resize(INTER_LINEAR)`
//!   的 half-pixel 坐标约定**（`src = (dst + 0.5) * scale - 0.5`），用于 det 输入缩放。
//! - [`normalize`] —— HWC u8 → CHW f32 归一化 `(x/255 - mean)/std`，SIMD 加速。
//! - [`sobel`] —— Sobel 边缘检测（M/N/H 三种），AVX2 快路径 + 标量回退。
//! - [`conv`] —— 5×5 加权卷积（AplyESS 高斯型 / AplyECP 十字型），AplyESS 用 AVX2。
//!
//! 数据用扁平 `&[u8]` / `&mut [u8]`（HWC 行优先），不引入 ndarray 依赖，由调用方
//! 负责包成自己的张量类型。SIMD 用 `wide` crate（stable 便携 SIMD，x86 走 SSE/AVX）
//! 与 `std::arch::x86_64` intrinsics（AVX2 快路径）。

// `#[target_feature]` 的 `unsafe fn` 内部直接调用 intrinsics（与既有 resize_avx2 一致）。
#![allow(unsafe_op_in_unsafe_fn)]

pub mod conv;
pub mod normalize;
pub mod resize;
pub mod sobel;

pub use conv::{aply_ecp, aply_ess, apply_moderate_threshold, zero_below_threshold};
pub use normalize::normalize_chw;
pub use resize::resize_bilinear_hwc;
pub use sobel::{sobel_h_edge, sobel_h_edge_into, sobel_m_edge, sobel_m_edge_into, sobel_n_edge, sobel_n_edge_into};

/// 从 `src` 的 `base` 起加载 16 个 u8 并加宽为 16 个 i16（lane 0..15）。
/// 调用方保证 `base+16 <= src.len()`。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn load16_i16(src: &[u8], base: usize) -> std::arch::x86_64::__m256i {
    use std::arch::x86_64::*;
    let bytes = _mm_loadu_si128(src.as_ptr().add(base) as *const __m128i); // 16 字节 = 16 u8
    _mm256_cvtepu8_epi16(bytes)
}
