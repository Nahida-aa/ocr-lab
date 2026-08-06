//! HWC u8 → CHW f32 归一化 `(x/255 - mean)/std`。

use wide::f32x8;

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
    fn normalize_value() {
        // 单像素 128，mean=0.5,std=0.5：out = (128/255 - 0.5)/0.5 ≈ 0.00392。
        let src = [128u8];
        let out = normalize_chw(&src, 1, 1, 1, &[0.5], &[0.5]);
        let expected = (128.0 / 255.0 - 0.5) / 0.5;
        assert!((out[0] - expected).abs() < 1e-5, "got {} expected {}", out[0], expected);
    }
}
