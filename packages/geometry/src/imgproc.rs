//! 纯 Rust 图像像素算子（SIMD 优化），替代/降低对 OpenCV 绑定在像素级处理上的依赖。
//!
//! 目前提供：
//! - `resize_bilinear_hwc` —— HWC u8 双线性缩放，**对齐 OpenCV `cv::resize(INTER_LINEAR)`
//!   的 half-pixel 坐标约定**（`src = (dst + 0.5) * scale - 0.5`），用于 det 输入缩放。
//! - `normalize_chw` —— HWC u8 → CHW f32 归一化 `(x/255 - mean)/std`，SIMD 加速。
//!
//! 数据用扁平 `&[u8]` / `&mut [u8]`（HWC 行优先），不引入 ndarray 依赖，由调用方
//! 负责包成自己的张量类型。SIMD 用 `wide` crate（stable 便携 SIMD，x86 走 SSE/AVX）。

use wide::f32x8;

/// HWC u8 双线性缩放（对齐 `cv::resize(INTER_LINEAR)` half-pixel 坐标）。
///
/// `src` 为 HWC 行优先（sw×sh×c），输出 `dw×dh×c`。坐标：对输出像素 (oy,ox)，
/// `sy = (oy+0.5)*sh/dh - 0.5`，`sx = (ox+0.5)*sw/dw - 0.5`，四邻加权（越界 clamp
/// 到边缘，等价 BORDER_REPLICATE）。
///
/// 实现分两段：
/// - 先按行/列预计算好所有 (sy,y0,y1,wy) 与 (sx,x0,x1,wx)，避免每像素重算；
/// - 主体用 `f32x8` 一次处理 8 个输出像素，SIMD 加速权重计算与加权求和。
pub fn resize_bilinear_hwc(
    src: &[u8],
    sw: usize,
    sh: usize,
    c: usize,
    dw: usize,
    dh: usize,
) -> Vec<u8> {
    assert_eq!(src.len(), sw * sh * c);
    let mut dst = vec![0u8; dw * dh * c];

    // 预计算行映射。
    let sy_scale = sh as f32 / dh as f32;
    let sx_scale = sw as f32 / dw as f32;
    let mut rows = vec![(0usize, 0usize, 0.0f32); dh];
    for oy in 0..dh {
        let sy = (oy as f32 + 0.5) * sy_scale - 0.5;
        let sy = sy.clamp(0.0, (sh - 1) as f32);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(sh - 1);
        rows[oy] = (y0, y1, sy - y0 as f32);
    }
    let mut cols = vec![(0usize, 0usize, 0.0f32); dw];
    for ox in 0..dw {
        let sx = (ox as f32 + 0.5) * sx_scale - 0.5;
        let sx = sx.clamp(0.0, (sw - 1) as f32);
        let x0 = sx.floor() as usize;
        let x1 = (x0 + 1).min(sw - 1);
        cols[ox] = (x0, x1, sx - x0 as f32);
    }

    // 逐通道，SIMD 一次 8 个输出像素。
    for ch in 0..c {
        let src_off = |y: usize, x: usize| (y * sw + x) * c + ch;
        // HWC 输出像素 (oy,ox) 的通道 ch 在 dst[(oy*dw+ox)*c + ch]。
        let dst_off = |oy: usize, ox: usize| (oy * dw + ox) * c + ch;
        for oy in 0..dh {
            let (y0, y1, wy) = rows[oy];
            // 每 8 个输出像素一向量。
            let mut ox = 0usize;
            while ox + 8 <= dw {
                // 取 8 个输出像素的列权重。
                let mut sx0 = [0usize; 8];
                let mut sx1 = [0usize; 8];
                let mut wx = [0.0f32; 8];
                for k in 0..8 {
                    let (a, b, w) = cols[ox + k];
                    sx0[k] = a;
                    sx1[k] = b;
                    wx[k] = w;
                }
                // 采集 4 个角点的 8 像素。
                let a = f32x8::from(collected(src, src_off, y0, &sx0));
                let b = f32x8::from(collected(src, src_off, y0, &sx1));
                let cc = f32x8::from(collected(src, src_off, y1, &sx0));
                let d = f32x8::from(collected(src, src_off, y1, &sx1));
                let wx8 = f32x8::from(wx);
                let wy8 = f32x8::splat(wy);
                // 双线性：top = a*(1-wx)+b*wx；bot = cc*(1-wx)+d*wx；out = top*(1-wy)+bot*wy
                let top = a.mul_add(-wx8, a) + b * wx8; // a*(1-wx)+b*wx
                let bot = cc.mul_add(-wx8, cc) + d * wx8;
                let out = top.mul_add(-wy8, top) + bot * wy8;
                let out: [f32; 8] = out.into();
                for k in 0..8 {
                    dst[dst_off(oy, ox + k)] = out[k].round().clamp(0.0, 255.0) as u8;
                }
                ox += 8;
            }
            // 尾部不足 8 个的标量处理。
            while ox < dw {
                let (x0, x1, wx) = cols[ox];
                let a = src[src_off(y0, x0)] as f32;
                let b = src[src_off(y0, x1)] as f32;
                let cc = src[src_off(y1, x0)] as f32;
                let d = src[src_off(y1, x1)] as f32;
                let top = a * (1.0 - wx) + b * wx;
                let bot = cc * (1.0 - wx) + d * wx;
                let val = top * (1.0 - wy) + bot * wy;
                dst[dst_off(oy, ox)] = val.round().clamp(0.0, 255.0) as u8;
                ox += 1;
            }
        }
    }
    dst
}

/// 按索引从 src 采集 8 个像素值（SIMD gather 的标量替代）。
fn collected(src: &[u8], off: impl Fn(usize, usize) -> usize, y: usize, xs: &[usize; 8]) -> [f32; 8] {
    let mut r = [0.0f32; 8];
    for k in 0..8 {
        r[k] = src[off(y, xs[k])] as f32;
    }
    r
}

/// HWC u8 → CHW f32 归一化 `(x/255 - mean)/std`。SIMD 加速。
///
/// 返回 CHW 扁平 `Vec<f32>`（c×h×w），`mean`/`std` 为各通道的 (mean, std)。
pub fn normalize_chw(src: &[u8], h: usize, w: usize, c: usize, mean: &[f32], std: &[f32]) -> Vec<f32> {
    assert_eq!(src.len(), h * w * c);
    assert_eq!(mean.len(), c);
    assert_eq!(std.len(), c);
    let mut dst = vec![0.0f32; c * h * w];
    // 归一化系数：out = a*px + b，其中 a=1/(255*std)，b=-mean/std（由 (x/255-mean)/std 展开）。
    let mut coef = vec![(0.0f32, 0.0f32); c];
    for ch in 0..c {
        let a = 1.0 / std[ch] / 255.0;
        let b = -mean[ch] / std[ch];
        coef[ch] = (a, b);
    }
    for ch in 0..c {
        let (a, b) = coef[ch];
        let a8 = f32x8::splat(a);
        let b8 = f32x8::splat(b);
        let out_off = ch * h * w;
        let src_off = ch; // HWC 中通道 ch 的元素相隔 c
        let mut i = 0usize;
        // 采集 HWC 里通道 ch 的连续像素（stride c）。
        let mut buf = [0.0f32; 8];
        while i + 8 <= h * w {
            for k in 0..8 {
                buf[k] = src[(i + k) * c + src_off] as f32;
            }
            let px = f32x8::from(buf);
            let out = px.mul_add(a8, b8);
            let out: [f32; 8] = out.into();
            for k in 0..8 {
                dst[out_off + i + k] = out[k];
            }
            i += 8;
        }
        while i < h * w {
            let px = src[i * c + src_off] as f32;
            dst[out_off + i] = px * a + b;
            i += 1;
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_identity() {
        // 2x2 单通道，resize 到同尺寸 = 原样。
        let src = [10u8, 20, 30, 40];
        let out = resize_bilinear_hwc(&src, 2, 2, 1, 2, 2);
        assert_eq!(out, src);
    }

    #[test]
    fn resize_single_pixel() {
        let src = [200u8];
        let out = resize_bilinear_hwc(&src, 1, 1, 1, 1, 1);
        assert_eq!(out, [200]);
    }

    #[test]
    fn normalize_value() {
        // 单像素 128，mean=0.5,std=0.5：out = (128/255 - 0.5)/0.5 ≈ 0.00392。
        let src = [128u8];
        let out = normalize_chw(&src, 1, 1, 1, &[0.5], &[0.5]);
        let expected = (128.0 / 255.0 - 0.5) / 0.5;
        assert!((out[0] - expected).abs() < 1e-5, "got {} expected {}", out[0], expected);
    }

    #[test]
    fn resize_bilinear_known_value() {
        // 4x1 灰度 resize 到 2x1。half-pixel：
        //   ox=0: sx=(0.5)*4/2-0.5=0.5 → (10+30)/2=20
        //   ox=1: sx=(1.5)*2-0.5=2.5 → (50+70)/2=60
        let src = [10u8, 30, 50, 70];
        let out = resize_bilinear_hwc(&src, 4, 1, 1, 2, 1);
        assert_eq!(out, [20, 60]);
    }

    #[test]
    fn resize_bilinear_downscale_known() {
        // 4x1 → 1x1：sx=(0.5)*4-0.5=1.5 → (30+50)/2=40
        let src = [10u8, 30, 50, 70];
        let out = resize_bilinear_hwc(&src, 4, 1, 1, 1, 1);
        assert_eq!(out, [40]);
    }

    #[test]
    fn resize_preserves_channels() {
        // 2x2 两通道：验证 HWC 通道不串扰。
        // 像素 (y,x): ch0 = 10*y+x, ch1 = 100+10*y+x
        let src = [
            10, 110, 11, 111, // y=0: x0,x1 的 ch0,ch1
            20, 120, 21, 121, // y=1
        ];
        let out = resize_bilinear_hwc(&src, 2, 2, 2, 2, 2);
        assert_eq!(out, src, "same-size resize 应保持每个通道值");
    }
}
