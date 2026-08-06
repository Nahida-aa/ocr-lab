//! 5×5 加权卷积（复刻 VideoSubFinder 的 `AplyESS` / `AplyECP`）。
//!
//! 输入 `u16` 边缘图 → 输出 `u16`。边界像素（最外 2 圈）保留 0。
//! AVX2 快路径按 8 像素向量化（u16→i32 加宽累加，与标量逐位一致）。

/// `ApplyModerateThreshold`：按全图最大值*mthr 阈值二值化（<thr→0，否则→255）。
/// 就地修改 `im`。AVX2 快路径（u16→i32 加宽比较，避免 u16>32767 的符号问题）。
pub fn apply_moderate_threshold(im: &mut [u16], mthr: f32) {
    let mx = im.iter().copied().max().unwrap_or(0);
    let thr = (mx as f32 * mthr) as u16;
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        unsafe { apply_moderate_threshold_avx2(im, thr) }
        return;
    }
    for v in im.iter_mut() {
        *v = if *v < thr { 0 } else { 255 };
    }
}

/// `ApplyModerateThreshold` AVX2：每 8 个 u16，加宽 i32 比较 thr，>=
/// thr 的 lane 置 255，否则 0。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn apply_moderate_threshold_avx2(im: &mut [u16], thr: u16) {
    use std::arch::x86_64::*;
    let thr_v = _mm256_set1_epi32(thr as i32);
    let mut i = 0usize;
    while i + 8 <= im.len() {
        let lo = _mm_loadu_si128(im.as_ptr().add(i) as *const __m128i); // 8 个 u16
        let v = _mm256_cvtepu16_epi32(lo); // 8 × i32
        // v >= thr → 255, else 0。cmpgt(v, thr-1) 等价 v >= thr。
        let ge = _mm256_cmpgt_epi32(v, _mm256_sub_epi32(thr_v, _mm256_set1_epi32(1)));
        // ge 位掩码 → 255。
        let out = _mm256_and_si256(ge, _mm256_set1_epi32(255));
        // 存 8 个 u32 的低 16 位 → 8 个 u16。
        let lo16 = _mm256_castsi256_si128(out); // 低 4 个 u32 的低 16 位 = 4 个 u16
        let hi16 = _mm256_extracti128_si256::<1>(out); // 高 4 个 u32 的低 16 位 = 4 个 u16
        // 拼接 8 个 u16 到连续 buffer。
        let mut tmp = [0u16; 8];
        _mm_storeu_si128(tmp.as_mut_ptr() as *mut __m128i, _mm_packus_epi32(lo16, lo16));
        let mut tmp_hi = [0u16; 4];
        _mm_storel_epi64(tmp_hi.as_mut_ptr() as *mut __m128i, _mm_packus_epi32(hi16, hi16));
        // lo16 的低 4 个 u16 + hi16 的低 4 个 u16。
        im[i..i + 4].copy_from_slice(&tmp[..4]);
        im[i + 4..i + 8].copy_from_slice(&tmp_hi);
        i += 8;
    }
    while i < im.len() {
        im[i] = if im[i] < thr { 0 } else { 255 };
        i += 1;
    }
}

/// `ZeroBelowThreshold`：`im[i] < thr` 的像素置 0，其余保留原值。就地。AVX2 加速。
pub fn zero_below_threshold(im: &mut [u16], thr: u16) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        unsafe { zero_below_threshold_avx2(im, thr) }
        return;
    }
    for v in im.iter_mut() {
        if *v < thr {
            *v = 0;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn zero_below_threshold_avx2(im: &mut [u16], thr: u16) {
    use std::arch::x86_64::*;
    let thr_v = _mm256_set1_epi32(thr as i32);
    let mut i = 0usize;
    while i + 8 <= im.len() {
        let lo = _mm_loadu_si128(im.as_ptr().add(i) as *const __m128i);
        let v = _mm256_cvtepu16_epi32(lo);
        // v < thr → 全 1 掩码；否则全 0。
        let lt = _mm256_cmpgt_epi32(thr_v, v);
        // blend：lt 位为 1 时取 0，否则取 v。用 andnot(v) + and(0) 等价：lt ? 0 : v。
        let masked = _mm256_andnot_si256(lt, v);
        // masked 的 8 个 i32（0 或原值）→ 8 个 u16。
        let mut vals = [0i32; 8];
        _mm256_storeu_si256(vals.as_mut_ptr() as *mut __m256i, masked);
        for k in 0..8 {
            im[i + k] = vals[k] as u16;
        }
        i += 8;
    }
    while i < im.len() {
        if im[i] < thr {
            im[i] = 0;
        }
        i += 1;
    }
}

/// `AplyESS`：5×5 高斯型加权平滑（系数 2/4/5/10/20/40，归一化 /220）。
/// 无数据依赖分支，可用 AVX2。
pub fn aply_ess(im_in: &[u16], w: usize, h: usize) -> Vec<u16> {
    let mut out = vec![0u16; w * h];
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
        {
            unsafe { aply_ess_avx512(im_in, w, h, &mut out) }
            return out;
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            unsafe { aply_ess_avx2(im_in, w, h, &mut out) }
            return out;
        }
    }
    aply_ess_scalar(im_in, w, h, &mut out);
    out
}

/// `AplyECP`：5×5 十字加权（系数 8/5/4/2/1，归一化 /100），仅对中心非 0 像素计算。
/// 有中心==0 分支（AVX2 用 blendv 掩码处理）。
pub fn aply_ecp(im_in: &[u16], w: usize, h: usize) -> Vec<u16> {
    let mut out = vec![0u16; w * h];
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
        {
            unsafe { aply_ecp_avx512(im_in, w, h, &mut out) }
            return out;
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            unsafe { aply_ecp_avx2(im_in, w, h, &mut out) }
            return out;
        }
    }
    aply_ecp_scalar(im_in, w, h, &mut out);
    out
}

fn aply_ess_scalar(im_in: &[u16], w: usize, h: usize, out: &mut [u16]) {
    let mx = w - 2;
    let my = h - 2;
    for y in 2..my {
        for x in 2..mx {
            let i = w * y + x;
            let val = 2i64
                * (im_in[i - w * 2 - 2] as i64
                    + im_in[i - w * 2 + 2] as i64
                    + im_in[i + w * 2 - 2] as i64
                    + im_in[i + w * 2 + 2] as i64)
                + 4i64
                    * (im_in[i - w * 2 - 1] as i64
                        + im_in[i - w * 2 + 1] as i64
                        + im_in[i - w - 2] as i64
                        + im_in[i - w + 2] as i64
                        + im_in[i + w - 2] as i64
                        + im_in[i + w + 2] as i64
                        + im_in[i + w * 2 - 1] as i64
                        + im_in[i + w * 2 + 1] as i64)
                + 5i64
                    * (im_in[i - w * 2] as i64
                        + im_in[i - 2] as i64
                        + im_in[i + 2] as i64
                        + im_in[i + w * 2] as i64)
                + 10i64
                    * (im_in[i - w - 1] as i64
                        + im_in[i - w + 1] as i64
                        + im_in[i + w - 1] as i64
                        + im_in[i + w + 1] as i64)
                + 20i64 * (im_in[i - w] as i64 + im_in[i - 1] as i64 + im_in[i + 1] as i64 + im_in[i + w] as i64)
                + 40i64 * im_in[i] as i64;
            out[i] = (val / 220) as u16;
        }
    }
}

fn aply_ecp_scalar(im_in: &[u16], w: usize, h: usize, out: &mut [u16]) {
    let mx = w - 2;
    let my = h - 2;
    for y in 2..my {
        for x in 2..mx {
            let i = w * y + x;
            if im_in[i] == 0 {
                out[i] = 0;
                continue;
            }
            let ii = i - ((w + 1) << 1);
            let mut val = 8i64 * im_in[ii] as i64
                + 5i64 * im_in[ii + 1] as i64
                + 4i64 * im_in[ii + 2] as i64
                + 5i64 * im_in[ii + 3] as i64
                + 8i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 5i64 * im_in[ii] as i64
                + 2i64 * im_in[ii + 1] as i64
                + im_in[ii + 2] as i64
                + 2i64 * im_in[ii + 3] as i64
                + 5i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 4i64 * im_in[ii] as i64 + im_in[ii + 1] as i64 + im_in[ii + 3] as i64 + 4i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 5i64 * im_in[ii] as i64
                + 2i64 * im_in[ii + 1] as i64
                + im_in[ii + 2] as i64
                + 2i64 * im_in[ii + 3] as i64
                + 5i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 8i64 * im_in[ii] as i64
                + 5i64 * im_in[ii + 1] as i64
                + 4i64 * im_in[ii + 2] as i64
                + 5i64 * im_in[ii + 3] as i64
                + 8i64 * im_in[ii + 4] as i64;
            out[i] = (val / 100) as u16;
        }
    }
}

/// `AplyECP` AVX2：5×5 十字卷积（核见下），中心==0 的像素用 blendv 掩码置 0。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn aply_ecp_avx2(im_in: &[u16], w: usize, h: usize, out: &mut [u16]) {    use std::arch::x86_64::*;
    let mx = w - 2;
    let my = h - 2;
    // ECP 5×5 权重核（row × col，-2..+2）；中心 (0,0) 权重 0。
    let kernel = [
        [8i32, 5, 4, 5, 8],
        [5i32, 2, 1, 2, 5],
        [4i32, 1, 0, 1, 4],
        [5i32, 2, 1, 2, 5],
        [8i32, 5, 4, 5, 8],
    ];
    for y in 2..my {
        let mut x = 2usize;
        while x + 8 <= mx {
            let base = y * w + x;
            let mut acc = _mm256_setzero_si256();
            let base_i = base as isize;
            for ri in 0..5 {
                let row_off = (ri as isize) - 2;
                for ci in 0..5 {
                    let wgt = kernel[ri][ci];
                    if wgt == 0 {
                        continue;
                    }
                    let col_off = (ci as isize) - 2;
                    let pos = (base_i + row_off * w as isize + col_off) as usize;
                    let v = load8_i32_u16(im_in, pos);
                    acc = _mm256_add_epi32(acc, _mm256_mullo_epi32(_mm256_set1_epi32(wgt), v));
                }
            }
            // 中心 (0,0) 权重 0：中心值不参与累加；中心==0 的像素整块置 0。
            let center = load8_i32_u16(im_in, base);
            let center_nonzero = _mm256_cmpgt_epi32(center, _mm256_setzero_si256());
            // 归一化 /100（AVX2 无整数除法，提取后标量除），并应用掩码。
            let mut vals = [0i32; 8];
            _mm256_storeu_si256(vals.as_mut_ptr() as *mut __m256i, acc);
            let mut cmask = [0i32; 8];
            _mm256_storeu_si256(cmask.as_mut_ptr() as *mut __m256i, center_nonzero);
            for k in 0..8 {
                let v = if cmask[k] != 0 { vals[k] / 100 } else { 0 };
                out[base + k] = v as u16;
            }
            x += 8;
        }
        // 尾部标量。
        while x < mx {
            let i = w * y + x;
            if im_in[i] == 0 {
                out[i] = 0;
                x += 1;
                continue;
            }
            let ii = i - ((w + 1) << 1);
            let mut val = 8i64 * im_in[ii] as i64
                + 5i64 * im_in[ii + 1] as i64
                + 4i64 * im_in[ii + 2] as i64
                + 5i64 * im_in[ii + 3] as i64
                + 8i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 5i64 * im_in[ii] as i64
                + 2i64 * im_in[ii + 1] as i64
                + im_in[ii + 2] as i64
                + 2i64 * im_in[ii + 3] as i64
                + 5i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 4i64 * im_in[ii] as i64 + im_in[ii + 1] as i64 + im_in[ii + 3] as i64 + 4i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 5i64 * im_in[ii] as i64
                + 2i64 * im_in[ii + 1] as i64
                + im_in[ii + 2] as i64
                + 2i64 * im_in[ii + 3] as i64
                + 5i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 8i64 * im_in[ii] as i64
                + 5i64 * im_in[ii + 1] as i64
                + 4i64 * im_in[ii + 2] as i64
                + 5i64 * im_in[ii + 3] as i64
                + 8i64 * im_in[ii + 4] as i64;
            out[i] = (val / 100) as u16;
            x += 1;
        }
    }
}

/// `AplyECP` AVX-512：每 16 像素，5×5 十字卷积（权重核同上）。相比 AVX2 一次处理
/// 8 像素，这里 16 像素/向量（zmm，i32 加宽），吞吐翻倍（瓶颈是 25 次 load+累加）。
/// /100 与中心==0 掩码仍标量做（占比小；Rust std::arch 无 AVX-512 整数除法）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512bw,avx512vl")]
unsafe fn aply_ecp_avx512(im_in: &[u16], w: usize, h: usize, out: &mut [u16]) {
    use std::arch::x86_64::*;
    let mx = w - 2;
    let my = h - 2;
    // ECP 5×5 权重核（row × col，-2..+2）；中心 (0,0) 权重 0。
    let kernel = [
        [8i32, 5, 4, 5, 8],
        [5i32, 2, 1, 2, 5],
        [4i32, 1, 0, 1, 4],
        [5i32, 2, 1, 2, 5],
        [8i32, 5, 4, 5, 8],
    ];
    for y in 2..my {
        let mut x = 2usize;
        while x + 16 <= mx {
            let base = y * w + x;
            let mut acc = _mm512_setzero_si512();
            let base_i = base as isize;
            for ri in 0..5 {
                let row_off = (ri as isize) - 2;
                for ci in 0..5 {
                    let wgt = kernel[ri][ci];
                    if wgt == 0 {
                        continue;
                    }
                    let col_off = (ci as isize) - 2;
                    let pos = (base_i + row_off * w as isize + col_off) as usize;
                    // 加载 16 个 u16（256 位）并加宽为 16 个 i32。
                    let bytes = _mm256_loadu_si256(im_in.as_ptr().add(pos) as *const __m256i);
                    let v = _mm512_cvtepu16_epi32(bytes);
                    acc = _mm512_add_epi32(acc, _mm512_mullo_epi32(_mm512_set1_epi32(wgt), v));
                }
            }
            // 中心==0 → 该像素输出 0。16 个 i32 累加值 → /100（标量除，AVX-512 无
            // 直接整数除法；除法占比小，瓶颈是上面 25 次 load+累加，已由 16 像素向量化）。
            let cb = _mm256_loadu_si256(im_in.as_ptr().add(base) as *const __m256i);
            let center_nonzero = _mm512_cmpgt_epi32_mask(_mm512_cvtepu16_epi32(cb), _mm512_setzero_si512());
            let mut vals = [0i32; 16];
            _mm512_storeu_si512(vals.as_mut_ptr() as *mut __m512i, acc);
            let mut out16 = [0u16; 16];
            for k in 0..16 {
                let v = if (center_nonzero >> k) & 1 != 0 { vals[k] / 100 } else { 0 };
                out16[k] = v as u16;
            }
            out[base..base + 16].copy_from_slice(&out16);
            x += 16;
        }
        // 尾部标量（与 AVX2 版一致）。
        while x < mx {
            let i = w * y + x;
            if im_in[i] == 0 {
                out[i] = 0;
                x += 1;
                continue;
            }
            let ii = i - ((w + 1) << 1);
            let mut val = 8i64 * im_in[ii] as i64
                + 5i64 * im_in[ii + 1] as i64
                + 4i64 * im_in[ii + 2] as i64
                + 5i64 * im_in[ii + 3] as i64
                + 8i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 5i64 * im_in[ii] as i64
                + 2i64 * im_in[ii + 1] as i64
                + im_in[ii + 2] as i64
                + 2i64 * im_in[ii + 3] as i64
                + 5i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 4i64 * im_in[ii] as i64 + im_in[ii + 1] as i64 + im_in[ii + 3] as i64 + 4i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 5i64 * im_in[ii] as i64
                + 2i64 * im_in[ii + 1] as i64
                + im_in[ii + 2] as i64
                + 2i64 * im_in[ii + 3] as i64
                + 5i64 * im_in[ii + 4] as i64;
            let ii = ii + w;
            val += 8i64 * im_in[ii] as i64
                + 5i64 * im_in[ii + 1] as i64
                + 4i64 * im_in[ii + 2] as i64
                + 5i64 * im_in[ii + 3] as i64
                + 8i64 * im_in[ii + 4] as i64;
            out[i] = (val / 100) as u16;
            x += 1;
        }
    }
}

/// 从 `im` 的 `base` 起加载 8 个 u16 并加宽为 8 个 i32（lane 0..7）。
/// 调用方保证 `base+8 <= im.len()`。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn load8_i32_u16(im: &[u16], base: usize) -> std::arch::x86_64::__m256i {
    use std::arch::x86_64::*;
    // 加载 128 位 = 8 个 u16，再加宽为 8 个 i32。
    let bytes = _mm_loadu_si128(im.as_ptr().add(base) as *const __m128i);
    _mm256_cvtepu16_epi32(bytes)
}

/// `AplyESS` AVX2：每 8 像素，5 行 stencil（每行取 -2,-1,0,1,2 偏移的 8 列），i32 累加。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn aply_ess_avx2(im_in: &[u16], w: usize, h: usize, out: &mut [u16]) {
    use std::arch::x86_64::*;
    let mx = w - 2;
    let my = h - 2;
    for y in 2..my {
        let mut x = 2usize;
        while x + 8 <= mx {
            let base = y * w + x;
            // 5×5 权重核（row_off × col_off，-2..+2）。
            let kernel = [
                [2i32, 4, 5, 4, 2],
                [4i32, 10, 20, 10, 4],
                [5i32, 20, 40, 20, 5],
                [4i32, 10, 20, 10, 4],
                [2i32, 4, 5, 4, 2],
            ];
            let mut acc = _mm256_setzero_si256();
            let base_i = base as isize;
            for ri in 0..5 {
                // 行偏移 -2..+2（kernel 行 0 对应 y-2）。
                let row_off = (ri as isize) - 2;
                for ci in 0..5 {
                    let col_off = (ci as isize) - 2;
                    let pos = (base_i + row_off * w as isize + col_off) as usize;
                    let v = load8_i32_u16(im_in, pos);
                    let wgt = _mm256_set1_epi32(kernel[ri][ci]);
                    acc = _mm256_add_epi32(acc, _mm256_mullo_epi32(wgt, v));
                }
            }
            // 8 个 i32 累加值 → 标量除以 220 存 u16（除法无 AVX2 整数指令，标量做）。
            let mut vals = [0i32; 8];
            _mm256_storeu_si256(vals.as_mut_ptr() as *mut __m256i, acc);
            for k in 0..8 {
                out[base + k] = (vals[k] / 220) as u16;
            }
            x += 8;
        }
        // 尾部标量。
        while x < mx {
            let i = w * y + x;
            let val = 2i64
                * (im_in[i - w * 2 - 2] as i64
                    + im_in[i - w * 2 + 2] as i64
                    + im_in[i + w * 2 - 2] as i64
                    + im_in[i + w * 2 + 2] as i64)
                + 4i64
                    * (im_in[i - w * 2 - 1] as i64
                        + im_in[i - w * 2 + 1] as i64
                        + im_in[i - w - 2] as i64
                        + im_in[i - w + 2] as i64
                        + im_in[i + w - 2] as i64
                        + im_in[i + w + 2] as i64
                        + im_in[i + w * 2 - 1] as i64
                        + im_in[i + w * 2 + 1] as i64)
                + 5i64
                    * (im_in[i - w * 2] as i64
                        + im_in[i - 2] as i64
                        + im_in[i + 2] as i64
                        + im_in[i + w * 2] as i64)
                + 10i64
                    * (im_in[i - w - 1] as i64
                        + im_in[i - w + 1] as i64
                        + im_in[i + w - 1] as i64
                        + im_in[i + w + 1] as i64)
                + 20i64 * (im_in[i - w] as i64 + im_in[i - 1] as i64 + im_in[i + 1] as i64 + im_in[i + w] as i64)
                + 40i64 * im_in[i] as i64;
            out[i] = (val / 220) as u16;
            x += 1;
        }
    }
}

/// `AplyESS` AVX-512：每 16 像素，5×5 高斯加权（核同上）。相比 AVX2 一次 8 像素，
/// 这里 16 像素/向量（zmm，i32 加宽），吞吐翻倍。ESS 无中心分支，直接累加。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512bw,avx512vl")]
unsafe fn aply_ess_avx512(im_in: &[u16], w: usize, h: usize, out: &mut [u16]) {
    use std::arch::x86_64::*;
    let mx = w - 2;
    let my = h - 2;
    // ESS 5×5 权重核（row × col，-2..+2）。
    let kernel = [
        [2i32, 4, 5, 4, 2],
        [4i32, 10, 20, 10, 4],
        [5i32, 20, 40, 20, 5],
        [4i32, 10, 20, 10, 4],
        [2i32, 4, 5, 4, 2],
    ];
    for y in 2..my {
        let mut x = 2usize;
        while x + 16 <= mx {
            let base = y * w + x;
            let mut acc = _mm512_setzero_si512();
            let base_i = base as isize;
            for ri in 0..5 {
                let row_off = (ri as isize) - 2;
                for ci in 0..5 {
                    let col_off = (ci as isize) - 2;
                    let pos = (base_i + row_off * w as isize + col_off) as usize;
                    let bytes = _mm256_loadu_si256(im_in.as_ptr().add(pos) as *const __m256i);
                    let v = _mm512_cvtepu16_epi32(bytes);
                    acc = _mm512_add_epi32(acc, _mm512_mullo_epi32(_mm512_set1_epi32(kernel[ri][ci]), v));
                }
            }
            // 16 个 i32 → /220（标量除；占比小，瓶颈是 25 次 load+累加）。
            let mut vals = [0i32; 16];
            _mm512_storeu_si512(vals.as_mut_ptr() as *mut __m512i, acc);
            let mut out16 = [0u16; 16];
            for k in 0..16 {
                out16[k] = (vals[k] / 220) as u16;
            }
            out[base..base + 16].copy_from_slice(&out16);
            x += 16;
        }
        // 尾部标量（与 AVX2 版一致）。
        while x < mx {
            let i = w * y + x;
            let val = 2i64
                * (im_in[i - w * 2 - 2] as i64
                    + im_in[i - w * 2 + 2] as i64
                    + im_in[i + w * 2 - 2] as i64
                    + im_in[i + w * 2 + 2] as i64)
                + 4i64
                    * (im_in[i - w * 2 - 1] as i64
                        + im_in[i - w * 2 + 1] as i64
                        + im_in[i - w - 2] as i64
                        + im_in[i - w + 2] as i64
                        + im_in[i + w - 2] as i64
                        + im_in[i + w + 2] as i64
                        + im_in[i + w * 2 - 1] as i64
                        + im_in[i + w * 2 + 1] as i64)
                + 5i64
                    * (im_in[i - w * 2] as i64
                        + im_in[i - 2] as i64
                        + im_in[i + 2] as i64
                        + im_in[i + w * 2] as i64)
                + 10i64
                    * (im_in[i - w - 1] as i64
                        + im_in[i - w + 1] as i64
                        + im_in[i + w - 1] as i64
                        + im_in[i + w + 1] as i64)
                + 20i64 * (im_in[i - w] as i64 + im_in[i - 1] as i64 + im_in[i + 1] as i64 + im_in[i + w] as i64)
                + 40i64 * im_in[i] as i64;
            out[i] = (val / 220) as u16;
            x += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image(w: usize, h: usize) -> Vec<u16> {
        let mut seed = 0x9e3779b9u32;
        let mut img = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                img[y * w + x] = ((seed >> 16) & 0xffff) as u16;
            }
        }
        img
    }

    #[test]
    fn aply_ess_simd_matches_scalar() {
        let (w, h) = (37usize, 19usize);
        let src = test_image(w, h);
        let mut scalar = vec![0u16; w * h];
        aply_ess_scalar(&src, w, h, &mut scalar);
        let simd = aply_ess(&src, w, h);
        assert_eq!(simd, scalar, "AplyESS: SIMD 与标量应逐位一致");
    }

    #[test]
    fn aply_ecp_simd_matches_scalar() {
        let (w, h) = (37usize, 19usize);
        // 含大量 0 值（测 center==0 分支）。
        let mut src = test_image(w, h);
        for i in (0..w * h).step_by(3) {
            src[i] = 0;
        }
        let mut scalar = vec![0u16; w * h];
        aply_ecp_scalar(&src, w, h, &mut scalar);
        let simd = aply_ecp(&src, w, h);
        assert_eq!(simd, scalar, "AplyECP: SIMD 与标量应逐位一致");
    }

    #[test]
    fn apply_moderate_threshold_simd_matches_scalar() {
        // 大值（>32767）测 u16 符号问题。
        let mut im = vec![0u16, 100, 40000, 255, 5000, 45055, 1, 32768, 200, 60000];
        let mut scalar = im.clone();
        let mthr = 0.3f32;
        apply_moderate_threshold(&mut im, mthr);
        // 标量参考。
        let mx = scalar.iter().copied().max().unwrap_or(0);
        let thr = (mx as f32 * mthr) as u16;
        for v in scalar.iter_mut() {
            *v = if *v < thr { 0 } else { 255 };
        }
        assert_eq!(im, scalar, "apply_moderate_threshold: SIMD 与标量应逐位一致");
    }

    #[test]
    fn zero_below_threshold_simd_matches_scalar() {
        let mut im = vec![0u16, 100, 40000, 255, 5000, 45055, 1, 32768, 200, 60000, 50, 999];
        let thr = 300u16;
        let mut scalar = im.clone();
        for v in scalar.iter_mut() {
            if *v < thr {
                *v = 0;
            }
        }
        zero_below_threshold(&mut im, thr);
        assert_eq!(im, scalar, "zero_below_threshold: SIMD 与标量应逐位一致");
    }

    /// 微型基准：标量 vs SIMD（release 下观察加速）。
    #[test]
    #[ignore] // 手动跑：cargo test -p geometry --release --lib aply_ess_bench -- --ignored --nocapture
    fn aply_ess_bench() {
        let (w, h) = (1280usize, 720usize);
        let src = test_image(w, h);
        let runs = 50;
        let t0 = std::time::Instant::now();
        for _ in 0..runs {
            let _ = aply_ess_scalar(&src, w, h, &mut vec![0u16; w * h]);
        }
        let s_ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        let t0 = std::time::Instant::now();
        for _ in 0..runs {
            let _ = aply_ess(&src, w, h);
        }
        let d_ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!("AplyESS 720p: scalar {:.2}ms, SIMD {:.2}ms, 加速 {:.1}x", s_ms, d_ms, s_ms / d_ms);

        // ECP 基准。
        let mut src_ecp = src.clone();
        for i in (0..w * h).step_by(2) {
            src_ecp[i] = 0; // 造 0 值测分支
        }
        let t0 = std::time::Instant::now();
        for _ in 0..runs {
            let _ = aply_ecp_scalar(&src_ecp, w, h, &mut vec![0u16; w * h]);
        }
        let s_ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        let t0 = std::time::Instant::now();
        for _ in 0..runs {
            let _ = aply_ecp(&src_ecp, w, h);
        }
        let d_ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!("AplyECP 720p: scalar {:.2}ms, SIMD {:.2}ms, 加速 {:.1}x", s_ms, d_ms, s_ms / d_ms);
    }
}
