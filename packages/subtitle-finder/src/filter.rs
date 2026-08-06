//! `FilterTransformedImage` 及其子函数（复刻 `IPAlgorithms.cpp`）。
//!
//! 输入 `ImFF`/`ImSF`（前景文字二值图）、`ImNE`（边缘图）、`LB/LE/N`（文字带）。
//! 用**连通域**（8 邻接）替代手写 `CMyClosedFigure`，实现：
//! - `SecondFiltration`：按 `segh` 高条带找水平白段，按边缘密度过滤噪声。
//! - `FilterImageByNotIntersectedFiguresWithImMask`：只保留与 mask 相交的连通域。
//! - `ClearImageFromSmallSymbols`：剔除过小连通域（小于 `msh`/`segh`）。
//! - `RestoreStillExistLines`：按行邻域恢复。
//! - `ExtendImFWithDataFromImNF`：用边缘图扩展文字行。
//!
//! `g_text_alignment` 默认为 `Any`，故 `SecondFiltration` 中对齐相关分支（Center/Left/Right）
//! 不生效；本实现只复刻 `Any` 路径。

use super::imgops;
use super::params::Params;

/// 一个连通域（8 邻接）及其边界框。
struct Figure {
    points: Vec<usize>,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

impl Figure {
    fn width(&self) -> i32 {
        self.max_x - self.min_x + 1
    }
    fn height(&self) -> i32 {
        self.max_y - self.min_y + 1
    }
}

/// 8-邻接连通域标注。`im` 中等于 `white` 的像素组成连通域。
/// 返回所有非空连通域（与 `SearchClosedFigures(combine_diagonal_points=true)` 等价）。
fn connected_components(im: &[u8], w: usize, h: usize, white: u8) -> Vec<Figure> {
    let size = w * h;
    let mut visited = vec![false; size];
    let mut comps: Vec<Figure> = Vec::new();
    // FIFO 队列（VecDeque）：BFS 按层展开，2D 图像行序访问，对缓存友好（行内像素
    // 连续命中同一 cache line）。原 LIFO 栈在 8 邻接下会跳跃，缓存命中率低。复用缓冲避免每次分配。
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::with_capacity(1024);
    for start in 0..size {
        if im[start] != white || visited[start] {
            continue;
        }
        // BFS。
        queue.clear();
        queue.push_back(start);
        visited[start] = true;
        let mut points = Vec::with_capacity(64);
        let mut min_x = w as i32;
        let mut max_x = 0i32;
        let mut min_y = h as i32;
        let mut max_y = 0i32;
        while let Some(p) = queue.pop_front() {
            points.push(p);
            let x = (p % w) as i32;
            let y = (p / w) as i32;
            if x < min_x {
                min_x = x;
            }
            if x > max_x {
                max_x = x;
            }
            if y < min_y {
                min_y = y;
            }
            if y > max_y {
                max_y = y;
            }
            // 8 邻域：避免重复边界检查，先算行列范围。
            let y0 = (y - 1).max(0);
            let y1 = (y + 1).min(h as i32 - 1);
            let x0 = (x - 1).max(0);
            let x1 = (x + 1).min(w as i32 - 1);
            for ny in y0..=y1 {
                for nx in x0..=x1 {
                    if nx == x && ny == y {
                        continue;
                    }
                    let ni = (ny as usize) * w + (nx as usize);
                    if im[ni] == white && !visited[ni] {
                        visited[ni] = true;
                        queue.push_back(ni);
                    }
                }
            }
        }
        comps.push(Figure {
            points,
            min_x,
            max_x,
            min_y,
            max_y,
        });
    }
    comps
}

/// `FilterImageByNotIntersectedFiguresWithImMask`：`ImInOut` 中与 `ImMASK` 不相交的
/// 连通域全部清除。就地改 `im_inout`。
fn filter_by_not_intersected_figures(im_inout: &mut [u8], im_mask: &[u8], w: usize, h: usize) {
    let figs = connected_components(im_inout, w, h, 255);
    for f in &figs {
        let mut found = false;
        for &p in &f.points {
            if im_mask[p] != 0 {
                found = true;
                break;
            }
        }
        if !found {
            for &p in &f.points {
                im_inout[p] = 0;
            }
        }
    }
}

/// `ClearImageFromSmallSymbols`：剔除过小的连通域。返回是否仍有连通域。
pub(crate) fn clear_image_from_small_symbols(im: &mut [u8], w: usize, h: usize, p: &Params) -> i32 {
    let figs = connected_components(im, w, h, 255);
    let n = figs.len();
    if n == 0 {
        return 0;
    }
    let segh = p.segh as i32;
    let msh_h = (p.msh * h as f32) as i32;
    let mut kept = 0i32;
    for f in &figs {
        if f.height() < msh_h || f.width() <= segh || f.height() <= segh {
            for &pt in &f.points {
                im[pt] = 0;
            }
        } else {
            kept += 1;
        }
    }
    if kept > 0 {
        1
    } else {
        0
    }
}

/// `RestoreStillExistLines`：把 `ImOrig` 中在已有文字行邻域（dy）内的行恢复进 `Im`。就地改 `im`。
fn restore_still_exist_lines(im: &mut [u8], im_orig: &[u8], w: usize, h: usize, p: &Params) {
    let dy = ((p.msh * h as f32) as i32) + 1;
    let mut lines_info = vec![0i32; h];
    for y in 0..h {
        let row = y * w;
        if im[row..row + w].iter().any(|&v| v != 0) {
            lines_info[y] = 1;
        }
    }
    for y in 0..h {
        let lo = (y as i32 - dy).max(0) as usize;
        let hi = ((y as i32 + dy).min(h as i32 - 1)) as usize;
        let found = (lo..=hi).any(|y2| lines_info[y2] == 1);
        if found {
            let row = y * w;
            im[row..row + w].copy_from_slice(&im_orig[row..row + w]);
        }
    }
}

/// `ExtendImFWithDataFromImNF`：把 `ImF` 中文字行的范围在 `ImNF`（边缘图）上扩展。就地改 `im_f`。
fn extend_imf_with_data_from_imnf(im_f: &mut [u8], im_nf: &[u8], w: usize, h: usize) {
    // 收集 ImF 中每行非 0 的水平范围，聚成文字行。
    let mut n: i32 = 0;
    let mut llb: Vec<i32> = vec![-1; h + 1];
    let mut lle: Vec<i32> = vec![-1; h + 1];
    let mut ll: Vec<i32> = vec![(w - 1) as i32; h + 1];
    let mut lr: Vec<i32> = vec![0; h + 1];
    llb[0] = -1;
    lle[0] = -1;
    ll[0] = (w - 1) as i32;
    lr[0] = 0;

    for y in 0..h {
        let ib = y * w;
        let mut bln = 0;
        let mut l = 0;
        let mut r = 0;
        for x in 0..w {
            if im_f[ib + x] != 0 {
                if llb[n as usize] == -1 {
                    llb[n as usize] = y as i32;
                    lle[n as usize] = y as i32;
                } else {
                    lle[n as usize] = y as i32;
                }
                if bln == 0 {
                    l = x as i32;
                    bln = 1;
                }
                r = x as i32;
            }
        }
        if bln == 0 && llb[n as usize] != -1 {
            n += 1;
            llb[n as usize] = -1;
            lle[n as usize] = -1;
            ll[n as usize] = (w - 1) as i32;
            lr[n as usize] = 0;
        }
        if bln == 1 {
            if ll[n as usize] > l {
                ll[n as usize] = l;
            }
            if lr[n as usize] < r {
                lr[n as usize] = r;
            }
        }
    }
    if lle[n as usize] == h as i32 - 1 {
        n += 1;
    }

    // 合并相近行。
    let mut k = n - 2;
    while k >= 0 {
        let gap = llb[(k + 1) as usize] - lle[k as usize] - 1;
        let h1 = lle[k as usize] - llb[k as usize] + 1;
        let h2 = lle[(k + 1) as usize] - llb[(k + 1) as usize] + 1;
        let w1 = lr[k as usize] - ll[k as usize] + 1;
        let w2 = lr[(k + 1) as usize] - ll[(k + 1) as usize] + 1;
        if gap <= h1.min(h2) && gap <= w1.min(w2) {
            lle[k as usize] = lle[(k + 1) as usize];
            ll[k as usize] = ll[k as usize].min(ll[(k + 1) as usize]);
            lr[k as usize] = lr[k as usize].max(lr[(k + 1) as usize]);
            for i in (k + 1)..(n - 1) {
                ll[i as usize] = ll[(i + 1) as usize];
                lr[i as usize] = lr[(i + 1) as usize];
                llb[i as usize] = llb[(i + 1) as usize];
                lle[i as usize] = lle[(i + 1) as usize];
            }
            n -= 1;
            if k == n - 1 {
                k -= 1;
            }
            continue;
        }
        k -= 1;
    }

    // 用 ImNF 扩展 ImF。
    for k in 0..n {
        for y in llb[k as usize]..=lle[k as usize] {
            for x in ll[k as usize]..=lr[k as usize] {
                let idx = y as usize * w + x as usize;
                if im_nf[idx] != 0 {
                    im_f[idx] = 255;
                }
            }
        }
    }
}

/// `SecondFiltration`（`Any` 对齐路径）：按 `segh` 高条带找水平白段，
/// 用 `ImNE` 边缘密度过滤。返回 1 表示保留了文字。就地改 `im`。
///
/// 复刻 `IPAlgorithms.cpp:1905`。注意 `Any` 默认下对齐相关分支（Center/Left/Right、
/// `btd` 段距、`mpd`/`mpned` 密度）不生效，核心是「段内 `ImNE` 边缘点数 >= `g_mpn`」。
#[allow(clippy::too_many_arguments)]
pub(crate) fn second_filtration(
    im: &mut [u8],
    im_ne: &[u8],
    lb: &[i32],
    le: &[i32],
    n: usize,
    w: usize,
    h: usize,
    p: &Params,
) -> i32 {
    let segh = p.segh;
    let mpn = p.mpn as i32;
    let mut res = 0i32;

    for k in 0..n {
        let ie = (lb[k] + (((le[k] + segh as i32).min(h as i32 - 1) - lb[k]) / segh as i32) * segh as i32) * w as i32;
        let da = (segh * w) as i32;
        let mut ia = lb[k] * w as i32;

        // 遍历带内每个 segh 高条带（C++：`for(ia=LB*w; ia<ie; ia+=da)`；Any 路径
        // 内层 while(1) 只处理一次即稳定）。
        while ia < ie {
            // 找该条带内的水平白段。
            let mut seg_lb = vec![0i32; w];
            let mut seg_le = vec![0i32; w];
            let mut bln = 0;
            let mut l = 0usize;
            let mut x = 0usize;
            while x < w {
                let mut has_white = false;
                for y in 0..segh {
                    let idx = (ia as usize + x) + y * w;
                    if im[idx] == 255 {
                        has_white = true;
                        break;
                    }
                }
                if has_white {
                    if bln == 0 {
                        seg_lb[l] = x as i32;
                        seg_le[l] = x as i32;
                        bln = 1;
                    } else {
                        seg_le[l] = x as i32;
                    }
                } else if bln == 1 {
                    bln = 0;
                    l += 1;
                }
                x += 1;
            }
            if bln == 1 {
                if seg_le[l] == w as i32 - 1 {
                    l += 1;
                }
            }
            let ln = l as i32;

            // Any 路径：内层 while(1) 一次。若无线段，跳过本带。
            if ln > 0 {
                // 统计段内 ImNE 边缘点数。
                let mut n_ne = 0i32;
                for y in 0..segh {
                    let ib = (ia as usize) + y * w;
                    for ll in 0..ln {
                        let mut i = ib + seg_lb[ll as usize] as usize;
                        let val = ib + seg_le[ll as usize] as usize;
                        while i <= val {
                            if im_ne[i] == 255 {
                                n_ne += 1;
                            }
                            i += 1;
                        }
                    }
                }

                if n_ne < mpn {
                    // 移除所有子段（边缘点数不足）。
                    for y in 0..segh {
                        let start = (ia as usize) + y * w + seg_lb[0] as usize;
                        let cnt = (seg_le[(ln - 1) as usize] - seg_lb[0] + 1) as usize;
                        im[start..start + cnt].iter_mut().for_each(|v| *v = 0);
                    }
                } else {
                    // nNE 达标：保留（C++ 中 ln==ln_orig 稳定即 res=1）。
                    res = 1;
                }
            }

            ia += da;
        }

        // 清除带底部未处理区域（C++：`Im.set_values(0, ia, (LE[k]-cur_y+1)*w)`）。
        let cur_y = ia / w as i32;
        if cur_y <= le[k] {
            let start = ia as usize;
            let cnt = ((le[k] - cur_y + 1) * w as i32) as usize;
            im[start..start + cnt].iter_mut().for_each(|v| *v = 0);
        }
    }

    res
}

/// `FilterTransformedImage`：完整过滤流程，返回是否有文字。
/// 输入 `(im_ff, im_sf, im_tf)` 对应 C++ `(ImFF, ImSF, ImTF)`，`im_ne` 边缘图。
/// 就地修改 `im_sf`（ImSF）与 `im_tf`（ImTF）。
#[allow(clippy::too_many_arguments)]
pub fn filter_transformed_image(
    _im_ff: &[u8],
    im_sf: &mut [u8],
    im_tf: &mut [u8],
    im_ne: &[u8],
    lb: &[i32],
    le: &[i32],
    n: usize,
    w: usize,
    h: usize,
    p: &Params,
) -> i32 {
    // 1) NE 膨胀后与 ImSF 取交集（C++：ImRES1 = dilate(ImNE)，ImSF = ImSF ∩ ImRES1，
    //    然后 ImRES1 = ImSF 作为「过滤前快照」供 RestoreStillExistLines / Extend 使用）。
    // C++ 用 g_min_h=12/720（FilterTransformedImage 中的全局），此前误用 p.msh=0.01 偏小，
    // 导致膨胀半径偏小（3 vs 6），噪声过滤不足。改用 p.min_h=12/720。
    let dil_iters = ((p.min_h * h as f32) as i32) / 2;
    let ne_dil = imgops::dilate(im_ne, w, h, dil_iters);
    tracing::trace!(
        dil_iters,
        ne_wc = im_ne.iter().filter(|&&v| v == 255).count(),
        dil_wc = ne_dil.iter().filter(|&&v| v == 255).count(),
        "filter_transformed_image: dilate(NE)"
    );
    let mut im_res1 = im_sf.to_vec();
    imgops::intersect_two_images_inplace(&mut im_res1, &ne_dil, 0u8);
    im_sf.copy_from_slice(&im_res1);
    let step1_wc = im_sf.iter().filter(|&&v| v == 255).count();

    // 2) 二次过滤（连通域边缘密度），就地改 ImSF。
    let mut res = second_filtration(im_sf, im_ne, lb, le, n, w, h, p);
    let step2_wc = im_sf.iter().filter(|&&v| v == 255).count();
    tracing::trace!(step1_wc, step2_wc, res, "filter_transformed_image: second_filtration");

    if res == 1 {
        // ImTF = ImSF。
        im_tf.copy_from_slice(im_sf);

        // 恢复仍存在的行（用过滤前快照 ImRES1）。
        restore_still_exist_lines(im_tf, &im_res1, w, h, p);

        // 只保留与 ImSF（过滤后）相交的连通域。
        let sf_snapshot = im_sf.to_vec();
        filter_by_not_intersected_figures(im_tf, &sf_snapshot, w, h);

        let im_res2 = im_tf.to_vec();

        // 清除过小符号。
        res = clear_image_from_small_symbols(im_tf, w, h, p);

        if res == 1 {
            // 再次恢复。
            restore_still_exist_lines(im_tf, &im_res2, w, h, p);
            // 用过滤前快照 ImRES1 扩展文字行。
            extend_imf_with_data_from_imnf(im_tf, &im_res1, w, h);
        }
    }

    res
}

/// `AnalizeImageForSubPresence`：判断 `im_isa`（ImIntS/SP）是否含字幕，并把 `im_isa`
/// 就地替换为过滤后版本（`g_replace_ISA_by_filtered_version=true`）。
///
/// 复刻 `SSAlgorithms.cpp` 61-132。`im_il` 为 ILA u16 图；`im_ne` 为边缘图。
/// 返回 1 表示有字幕。对齐路径用 `LB=[0], LE=[h-1], N=1`。
pub(crate) fn analize_for_sub_presence(
    im_ne: &[u8],
    im_isa: &mut [u8],
    im_il: &[u16],
    w: usize,
    h: usize,
    p: &Params,
) -> i32 {
    let size = w * h;
    let mut im_ff = im_isa.to_vec();
    let mut im_tf = vec![0u8; size];

    // 与 ILA 交集（默认无颜色范围 → 直接交集）。
    imgops::intersect_two_images_inplace(&mut im_ff, im_il, 0u8);

    // ImSF = ImFF，然后 FilterTransformedImage。
    let mut im_sf = im_ff.clone();
    let lb = vec![0i32];
    let le = vec![(h as i32 - 1)];
    let res = filter_transformed_image(&im_ff, &mut im_sf, &mut im_tf, im_ne, &lb, &le, 1, w, h, p);

    // g_replace_ISA_by_filtered_version = true：ImISA = ImTF。
    im_isa.copy_from_slice(&im_tf);

    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params::default()
    }

    #[test]
    fn connected_components_finds_8adjacent() {
        let (w, h) = (4, 4);
        let mut im = vec![0u8; w * h];
        // 对角相邻的两个像素（8 邻接）应属同一连通域。
        im[0 * w + 0] = 255;
        im[1 * w + 1] = 255;
        // 孤立像素。
        im[3 * w + 3] = 255;
        let figs = connected_components(&im, w, h, 255);
        assert_eq!(figs.len(), 2, "应对角合并为 1 个 + 孤立 1 个");
        // 第一个包含两个像素，边界框 2x2。
        assert_eq!(figs[0].points.len(), 2);
        assert_eq!(figs[0].width(), 2);
        assert_eq!(figs[0].height(), 2);
    }

    #[test]
    fn clear_small_symbols_removes_tiny() {
        let (w, h) = (30, 30);
        let mut im = vec![0u8; w * h];
        // 一个 1x1 小点（应被清）。
        im[5 * w + 5] = 255;
        // 一个 4x10 的大块（应保留）。
        for y in 10..14 {
            for x in 10..20 {
                im[y * w + x] = 255;
            }
        }
        let res = clear_image_from_small_symbols(&mut im, w, h, &params());
        assert_eq!(res, 1, "大块保留");
        assert_eq!(im[5 * w + 5], 0, "小点被清除");
        assert_eq!(im[11 * w + 15], 255, "大块保留");
    }

    #[test]
    fn second_filtration_removes_low_edge_density() {
        // 一条白带，但 ImNE 几乎没有边缘 → 应被清除（nNE < mpn=50）。
        let (w, h) = (64, 20);
        let segh = params().segh;
        let mut im = vec![0u8; w * h];
        // 第 4..6 行白带。
        for y in 4..(4 + segh) {
            for x in 10..54 {
                im[y * w + x] = 255;
            }
        }
        let im_ne = vec![0u8; w * h]; // 无边缘
        let (lb, le) = (vec![0], vec![h as i32 - 1]);
        let mut im2 = im.clone();
        let res = second_filtration(&mut im2, &im_ne, &lb, &le, 1, w, h, &params());
        // 低边缘密度 → res=0 且白带被清。
        assert_eq!(res, 0);
        assert!(im2.iter().all(|&v| v == 0), "低边缘密度带应被清除");
    }

    #[test]
    fn second_filtration_keeps_high_edge_density() {
        // 白带内部有大量边缘（ImNE 全白）→ nNE 足够 → res=1。
        let (w, h) = (64, 20);
        let segh = params().segh;
        let mut im = vec![0u8; w * h];
        for y in 4..(4 + segh) {
            for x in 10..54 {
                im[y * w + x] = 255;
            }
        }
        let mut im_ne = vec![0u8; w * h];
        // 在白带内放满边缘。
        for y in 4..(4 + segh) {
            for x in 10..54 {
                im_ne[y * w + x] = 255;
            }
        }
        let (lb, le) = (vec![0], vec![h as i32 - 1]);
        let mut im2 = im.clone();
        let res = second_filtration(&mut im2, &im_ne, &lb, &le, 1, w, h, &params());
        assert_eq!(res, 1, "高边缘密度应保留");
        assert!(im2.iter().any(|&v| v == 255), "文字应保留");
    }

    #[test]
    fn filter_transformed_image_pipeline() {
        // 端到端：构造含文字（高对比条纹 + 边缘）的帧，过滤后应保留文字。
        let (w, h) = (64, 48);
        let mut im_sf = vec![0u8; w * h];
        let mut im_ne = vec![0u8; w * h];
        // 文字：第 20..26 行，条纹状白块（笔画宽 6px，保证连通域宽 > segh=3）。
        for y in 20..26 {
            for x in 10..54 {
                if (x - 10) % 8 < 6 {
                    im_sf[y * w + x] = 255;
                    im_ne[y * w + x] = 255;
                }
            }
        }
        let (lb, le) = (vec![18], vec![28]);
        let mut im_tf = vec![0u8; w * h];
        let res = filter_transformed_image(&[], &mut im_sf, &mut im_tf, &im_ne, &lb, &le, 1, w, h, &params());
        assert_eq!(res, 1, "应有文字");
        assert!(im_tf.iter().any(|&v| v == 255), "ImTF 应保留文字");
    }
}
