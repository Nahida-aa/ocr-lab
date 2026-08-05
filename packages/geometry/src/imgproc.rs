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
/// 实现分三段（OpenCV 的可分离双线性）：
/// - 预计算行/列映射；
/// - **水平插值**：对每条被引用的源行，把该行水平插值成 `dw` 宽的 f32 中间行，
///   用行缓存避免重复（多个输出行共享同一 y0/y1 时只算一次）；
/// - **垂直插值**：两行中间结果按 wy 混合（纯 SIMD，连续 load）。
/// 这样每输出像素只需 1 次取值（水平插值阶段），垂直阶段连续 SIMD。
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

    // 预计算行映射（垂直）。
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
    // 预计算列映射（水平）：每输出列 ox 的 (x0, x1, wx)。
    let mut cols = vec![(0usize, 0usize, 0.0f32); dw];
    for ox in 0..dw {
        let sx = (ox as f32 + 0.5) * sx_scale - 0.5;
        let sx = sx.clamp(0.0, (sw - 1) as f32);
        let x0 = sx.floor() as usize;
        let x1 = (x0 + 1).min(sw - 1);
        cols[ox] = (x0, x1, sx - x0 as f32);
    }

    // 水平插值：把源行 y 的通道 ch 水平插值成 f32[dw]。
    // SIMD：对连续的输出列，源行内 load 是连续的（同 channel，stride c），
    // 用 f32x8 一次算 8 列（取 x0/x1 位置的值仍标量 gather，但每源行只算一次）。
    fn interp_row(src: &[u8], sw: usize, c: usize, y: usize, ch: usize, cols: &[(usize, usize, f32)]) -> Vec<f32> {
        let dw = cols.len();
        let row_base = y * sw;
        let mut row = vec![0.0f32; dw];
        // SIMD 主体：一次 8 列。gather 用标量收集 x0/x1 的 8 个值。
        let mut ox = 0usize;
        while ox + 8 <= dw {
            let mut a0 = [0.0f32; 8];
            let mut a1 = [0.0f32; 8];
            let mut wx = [0.0f32; 8];
            for k in 0..8 {
                let (x0, x1, w) = cols[ox + k];
                a0[k] = src[(row_base + x0) * c + ch] as f32;
                a1[k] = src[(row_base + x1) * c + ch] as f32;
                wx[k] = w;
            }
            let a0v = f32x8::from(a0);
            let a1v = f32x8::from(a1);
            let wxv = f32x8::from(wx);
            let out = a0v.mul_add(-wxv, a0v) + a1v * wxv; // a0*(1-wx)+a1*wx
            let out: [f32; 8] = out.into();
            for k in 0..8 {
                row[ox + k] = out[k];
            }
            ox += 8;
        }
        // 尾部标量。
        while ox < dw {
            let (x0, x1, w) = cols[ox];
            let a0 = src[(row_base + x0) * c + ch] as f32;
            let a1 = src[(row_base + x1) * c + ch] as f32;
            row[ox] = a0 * (1.0 - w) + a1 * w;
            ox += 1;
        }
        row
    }

    // 行缓存：源行 y -> 水平插值结果（f32[dw]）。不同 ch 复用同一个缓存数组。
    // 注意：每通道 ch 需要独立缓存（值不同），这里在 ch 循环内重建。
    for ch in 0..c {
        // 缓存：row_cache[y] 存放源行 y 的水平插值（若 None 则未算）。
        let mut row_cache: Vec<Option<Vec<f32>>> = vec![None; sh];
        for oy in 0..dh {
            let (y0, y1, wy) = rows[oy];
            if row_cache[y0].is_none() {
                row_cache[y0] = Some(interp_row(src, sw, c, y0, ch, &cols));
            }
            if row_cache[y1].is_none() {
                row_cache[y1] = Some(interp_row(src, sw, c, y1, ch, &cols));
            }
            let r0 = row_cache[y0].as_ref().unwrap();
            let r1 = row_cache[y1].as_ref().unwrap();
            let wyv = f32x8::splat(wy);
            let one = f32x8::splat(1.0);
            // 垂直混合，SIMD 连续。
            let mut ox = 0usize;
            while ox + 8 <= dw {
                let a = f32x8::from([r0[ox], r0[ox + 1], r0[ox + 2], r0[ox + 3], r0[ox + 4], r0[ox + 5], r0[ox + 6], r0[ox + 7]]);
                let b = f32x8::from([r1[ox], r1[ox + 1], r1[ox + 2], r1[ox + 3], r1[ox + 4], r1[ox + 5], r1[ox + 6], r1[ox + 7]]);
                let out = a.mul_add(one - wyv, b * wyv); // a*(1-wy)+b*wy
                let out: [f32; 8] = out.into();
                for k in 0..8 {
                    dst[(oy * dw + ox + k) * c + ch] = out[k].round().clamp(0.0, 255.0) as u8;
                }
                ox += 8;
            }
            while ox < dw {
                let val = r0[ox] * (1.0 - wy) + r1[ox] * wy;
                dst[(oy * dw + ox) * c + ch] = val.round().clamp(0.0, 255.0) as u8;
                ox += 1;
            }
        }
    }
    dst
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

    #[test]
    fn resize_vertical_downscale() {
        // 2x3 单通道 → 2x1，验证垂直缩放。
        //   src: y0=[10,20], y1=[30,40], y2=[50,60]（2 宽 3 高）
        let src2 = [10u8, 20, 30, 40, 50, 60];
        let out2 = resize_bilinear_hwc(&src2, 2, 3, 1, 2, 1);
        // oy=0: sy=(0.5)*3/1-0.5=1.0 → y0=1,y1=min(2,2)=2,wy=0 → 源行1 = [30,40]
        assert_eq!(out2, [30, 40], "垂直 half-pixel 应取到中间行");
    }

    #[test]
    fn resize_horizontal_interpolate() {
        // 3x1 灰度 → 1x1：sx=(0.5)*3-0.5=1.0 → 源 x=1 → 30
        let src = [10u8, 20, 30];
        let out = resize_bilinear_hwc(&src, 3, 1, 1, 1, 1);
        assert_eq!(out, [20], "half-pixel sx=1.0 应取源像素 x=1");
    }
}
