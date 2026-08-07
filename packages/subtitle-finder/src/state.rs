//! 字幕时间轴状态机：FastSearchSubtitles。
//!
//! 复刻 VideoSubFinder 的核心状态机：用 `bf/ef`（起止帧）、`bt/et`（起止时间）、
//! `DL`、`max_dl_down/up` 跟踪字幕段，只在「字幕内容变化」时输出关键帧。
//! 这是「时间无偏移」的关键，必须精确对齐 C++ `FastSearchSubtitles`。

use std::path::Path;

use anyhow::Result;
use tracing::trace;

use crate::compare;
use crate::filter;
use crate::frame;
use crate::imgops;
use crate::params::Params;
use crate::Keyframe;

/// 一帧的解码 + 变换产物。
#[derive(Clone)]
pub(crate) struct FrameData {
    pub bgr: Vec<u8>,
    /// 去背景字幕前景图 ImTF（ISA，0/255）。
    pub im: Vec<u8>,
    /// 边缘图 ImNE（N+HE 并集）。
    pub ne: Vec<u8>,
    /// ILA 时间图（Y+255 的 u16）。
    pub y: Vec<u16>,
    /// 帧时间（ms）。
    pub pos: i64,
    /// 是否有字幕（FilterTransformedImage 的 has_text）。
    pub has_text: bool,
}

/// 顺序解码 + 变换的滑动窗口缓存（对应 C++ `RunSearch` 的环形缓冲）。
///
/// 用 [`frame::FrameStepper`] 流式解码：状态机 `fn` 单调推进时按需 `advance_to`，
/// 窗口只保留最近若干帧（对齐 C++ `m_N ≈ DL+threads`），避免全量驻留内存
/// （否则 170s/5100 帧 ≈ 30GB 会 OOM）。
pub struct FrameCache<'a> {
    path: &'a Path,
    p: &'a Params,
    w: usize,
    h: usize,
    prof: Option<imgops::Profiler>,
    stepper: Option<frame::FrameStepper>,
    /// 滑动窗口内容；索引 = fn - window_start。
    window: std::collections::VecDeque<FrameData>,
    window_start: i32,
    /// 累计解码帧数（EOF 后固定 = 视频总帧数）。
    decoded_total: i32,
}

/// 窗口保留的最大帧数（覆盖状态机访问 [fn-1, fn+3*DL]，DL=6 → ~20 帧）。
const MAX_WINDOW: i32 = 3 * 6 + 2;

/// 状态机每次推进 `fn` 时的前瞻解码帧数（覆盖 get_intersect_images 的 [fn, fn+DL-1]）。
const FORWARD: i32 = 3 * 6; // = 18

impl<'a> FrameCache<'a> {
    pub fn new(path: &'a Path, p: &'a Params) -> Self {
        Self {
            path,
            p,
            w: 0,
            h: 0,
            prof: None,
            stepper: None,
            window: std::collections::VecDeque::new(),
            window_start: 0,
            decoded_total: 0,
        }
    }

    /// 开启分阶段计时（性能剖析）。
    pub fn with_profiling(mut self) -> Self {
        let mut prof = imgops::Profiler::new();
        prof.enable();
        self.prof = Some(prof);
        self
    }

    /// 取剖析计时器。
    pub fn profiler(&self) -> Option<&imgops::Profiler> {
        self.prof.as_ref()
    }

    /// 惰性打开流式解码器。
    fn open_stepper(&mut self) -> Result<()> {
        if self.stepper.is_none() {
            self.stepper = Some(frame::FrameStepper::open(self.path)?);
        }
        Ok(())
    }

    /// 推进解码窗口，确保 [window_start, target] 已解码，并丢弃窗口外的旧帧。
    /// EOF 后 `decoded_total` 固定为视频总帧数。
    pub fn advance_to(&mut self, target: i32) -> Result<()> {
        self.open_stepper()?;
        let stepper = self.stepper.as_mut().expect("stepper 已打开");
        // 逐步解码到 target（或 EOF）。
        while self.decoded_total <= target {
            match stepper.next()? {
                Some((bgr, pts_ms)) => {
                    let (ch, cw) = (bgr.dim().0 as usize, bgr.dim().1 as usize);
                    if self.w == 0 {
                        self.w = cw;
                        self.h = ch;
                    }
                    let (w, h) = (self.w, self.h);
                    let mut flat = Vec::with_capacity(w * h * 3);
                    for y in 0..h {
                        for x in 0..w {
                            flat.push(bgr[[y, x, 0]]);
                            flat.push(bgr[[y, x, 1]]);
                            flat.push(bgr[[y, x, 2]]);
                        }
                    }
                    let n = self.decoded_total as usize;
                    let (_ff, _sf, im_tf, im_ne, im_y, _lb, _le, _n, has_text) =
                        imgops::get_transformed_image(&flat, w, h, self.p, self.prof.as_mut());
                    let y: Vec<u16> = if has_text == 1 {
                        im_y.iter().map(|&v| v as u16 + 255).collect()
                    } else {
                        vec![0; w * h]
                    };
                    trace!(
                        frame = n,
                        pos = pts_ms,
                        has_text,
                        isa_wc = im_tf.iter().filter(|&&v| v == 255).count(),
                        "decode frame"
                    );
                    self.window.push_back(FrameData {
                        bgr: flat,
                        im: im_tf,
                        ne: im_ne,
                        y,
                        pos: pts_ms, // 真实 PTS（毫秒），由 FrameStepper 换算。
                        has_text: has_text == 1,
                    });
                    self.decoded_total += 1;
                }
                None => {
                    // EOF：decoded_total 固定，不再推进。
                    break;
                }
            }
        }
        // 清理窗口：只保留 [target - MAX_WINDOW, target]。
        let keep_from = target - MAX_WINDOW;
        while !self.window.is_empty() && self.window_start < keep_from {
            self.window.pop_front();
            self.window_start += 1;
        }
        Ok(())
    }

    pub fn w(&self) -> usize {
        self.w
    }
    pub fn h(&self) -> usize {
        self.h
    }
    pub fn params(&self) -> &Params {
        self.p
    }
    /// 累计解码帧数（EOF 后 = 视频总帧数）。
    pub fn len(&self) -> usize {
        self.decoded_total as usize
    }
    pub fn is_empty(&self) -> bool {
        self.decoded_total == 0
    }
}

/// 取帧，越界/已被窗口丢弃返回 `None`（对应 C++ 帧不可得，状态机据此优雅结束）。
fn try_frame<'a>(cache: &'a FrameCache<'a>, fn_: i32) -> Option<&'a FrameData> {
    if fn_ < 0 || fn_ < cache.window_start {
        return None;
    }
    let idx = (fn_ - cache.window_start) as usize;
    if idx >= cache.window.len() {
        return None;
    }
    cache.window.get(idx)
}

fn get_frame<'a>(cache: &'a FrameCache<'a>, fn_: i32) -> Result<&'a FrameData> {
    try_frame(cache, fn_).ok_or_else(|| anyhow::anyhow!("帧 {} 越界", fn_))
}

/// `AnalizeImageForSubPresence`：判断交集 ISA 图是否含字幕。返回 bool。
/// 先把 ILA（u16 时间图）应用到 ISA（u8 前景），再转 `Array2` 调用 `analyse_image`。
pub(crate) fn analyse_image_flat(im: &[u8], ila: Option<&[u16]>, w: usize, h: usize, p: &Params) -> bool {
    let mut isa = im.to_vec();
    if let Some(il) = ila {
        imgops::intersect_two_images_inplace(&mut isa, il, 0u8);
    }
    let arr = ndarray::Array2::from_shape_vec((h, w), isa).expect("ISA 尺寸错误");
    crate::preprocess::analyse_image(&arr, p)
}

/// `IntersectImages`（多图版）：`im_res = ∩ ims[min..=max]`。就地。
fn intersect_images_range(im_res: &mut [u8], ims: &[&[u8]], min_id: usize, max_id: usize, w: usize, h: usize) {
    let size = w * h;
    for i in 0..size {
        if im_res[i] == 255 {
            for &im in &ims[min_id..=max_id] {
                if im[i] != 255 {
                    im_res[i] = 0;
                    break;
                }
            }
        }
    }
}

/// `IntersectYImages`（多图版）：`ImRes` 与 `ImY[min..=max]` 做时间交叠检查。就地。
fn intersect_y_images_range(im_res: &mut [u16], ims: &[&[u16]], min_id: usize, max_id: usize, p: &Params) {
    let size = im_res.len();
    for i in 0..size {
        if im_res[i] != 0 {
            let r = im_res[i] as i32;
            for &im in &ims[min_id..=max_id] {
                let v = im[i] as i32;
                if v < r - p.max_dl_down as i32 || v > r + p.max_dl_up as i32 {
                    im_res[i] = 0;
                    break;
                }
            }
        }
    }
}

/// `GetIntersectImages(fn)` 等价：`ImInt = ImForward[fn..fn+DL-1]` 交集 + `AnalyseImage`。
/// 返回 `(im_int, y_int, bln)`；帧越界返回 `None`（对应 C++ 帧不可得）。
pub(crate) fn get_intersect_images(
    cache: &FrameCache,
    fn_: usize,
) -> Option<(Vec<u8>, Vec<u16>, bool)> {
    let p = cache.params();
    let w = cache.w();
    let h = cache.h();
    let dl = p.dl;

    // 收集 [fn, fn+DL-1] 中**有字幕**（has_text）的帧（只存引用切片，不整帧拷贝）。
    // 第一帧越界 → 真·无数据（返回 None）。只对 has_text=1 的帧做交集：空字幕帧
    // （has_text=0）不参与，避免把交集清空导致段提前结束（C++ 异步下同样能看到
    // 段延伸到最后一个有字幕帧）。全部无字幕 → bln=false。
    let f0 = try_frame(cache, fn_ as i32)?;
    let mut ims: Vec<&[u8]> = Vec::with_capacity(dl);
    let mut imys: Vec<&[u16]> = Vec::with_capacity(dl);
    if f0.has_text {
        ims.push(&f0.im);
        imys.push(&f0.y);
    }
    for i in 1..dl {
        match try_frame(cache, (fn_ + i) as i32) {
            Some(f) if f.has_text => {
                ims.push(&f.im);
                imys.push(&f.y);
            }
            Some(_) => {} // 无字幕帧跳过
            None => break,
        }
    }
    if ims.is_empty() {
        // 窗口内无字幕帧 → bln=false（段结束）。
        return Some((vec![0u8; w * h], vec![0u16; w * h], false));
    }

    // 用第一帧数据作为工作缓冲，就地与其余帧相交（避免重复整帧拷贝）。
    let mut im_int = ims[0].to_vec();
    intersect_images_range(&mut im_int, &ims, 1, ims.len() - 1, w, h);

    let mut y_int = imys[0].to_vec();
    intersect_y_images_range(&mut y_int, &imys, 1, imys.len() - 1, p);

    let bln = analyse_image_flat(&im_int, Some(&y_int), w, h, p);
    Some((im_int, y_int, bln))
}

/// `CompareTwoSubsByOffset`：把当前字幕段 `(im_int_s, y_s, ne_s)` 与 `ImForward[offset]`
/// 比较，判断是否内容变化（返回 false 表示变化）。帧越界返回 `None`。
#[allow(clippy::too_many_arguments)]
pub(crate) fn compare_by_offset(
    cache: &FrameCache,
    fn_: usize,
    im_int_s: &[u8],
    y_s: &[u16],
    ne_s: &[u8],
    prev_ne: &[u8],
    offset: usize,
) -> Option<bool> {
    let p = cache.params();
    let w = cache.w();
    let h = cache.h();
    let dl = p.dl;

    let ne12 = if offset == 0 {
        prev_ne.to_vec()
    } else {
        try_frame(cache, (fn_ + offset - 1) as i32)?.ne.clone()
    };

    let f_off = try_frame(cache, (fn_ + offset) as i32)?;
    let mut bln = compare::compare_two_subs_optimal(
        im_int_s,
        Some(y_s),
        ne_s,
        Some(&ne12),
        &f_off.im,
        None,
        &f_off.ne,
        w,
        h,
        0,
        w as i32 - 1,
        p,
    );

    if !bln {
        // 交集 ImInt2 = ImForward[offset..DL-2]。
        let mut im_int2 = f_off.im.clone();
        let mut y_int2 = f_off.y.clone();
        // 收集 offset+1..=DL-2 帧。
        let mut ims: Vec<Vec<u8>> = Vec::new();
        let mut imys: Vec<Vec<u16>> = Vec::new();
        for i in (offset + 1)..=(dl - 2) {
            let f = try_frame(cache, (fn_ + i) as i32)?;
            ims.push(f.im.clone());
            imys.push(f.y.clone());
        }
        let im_refs: Vec<&[u8]> = ims.iter().map(|v| v.as_slice()).collect();
        let iy_refs: Vec<&[u16]> = imys.iter().map(|v| v.as_slice()).collect();
        if !im_refs.is_empty() {
            intersect_images_range(&mut im_int2, &im_refs, 0, im_refs.len() - 1, w, h);
            intersect_y_images_range(&mut y_int2, &iy_refs, 0, iy_refs.len() - 1, p);
        }
        bln = compare::compare_two_subs_optimal(
            im_int_s,
            Some(y_s),
            ne_s,
            Some(&ne12),
            &im_int2,
            Some(&y_int2),
            &f_off.ne,
            w,
            h,
            0,
            w as i32 - 1,
            p,
        );
    }

    Some(bln)
}

/// `FindOffsetForNewSub`：找第一个与当前字幕段内容不同的 forward offset。返回 offset。
#[allow(clippy::too_many_arguments)]
pub(crate) fn find_offset_for_new_sub(
    cache: &FrameCache,
    fn_: usize,
    im_int_s: &[u8],
    y_s: &[u16],
    ne_s: &[u8],
    prev_ne: &[u8],
) -> Option<usize> {
    let dl = cache.params().dl;
    for offset in 0..(dl - 1) {
        let same = compare_by_offset(cache, fn_, im_int_s, y_s, ne_s, prev_ne, offset)?;
        if !same {
            return Some(offset);
        }
    }
    Some(dl - 1)
}

/// 顶层入口：解码视频并跑状态机。
pub fn find_keyframes(video: &Path, params: &Params) -> Result<Vec<Keyframe>> {
    let mut cache = FrameCache::new(video, params);
    find_keyframes_with_cache(&mut cache, params)
}

/// 用已构造的缓存跑状态机。
///
/// 不再全量解码：先解码第一帧确定维度，再由 [`run_state_machine`] 在推进 `fn`
/// 时按需 `advance_to` 逐帧解码（滑动窗口）。
pub fn find_keyframes_with_cache(
    cache: &mut FrameCache,
    params: &Params,
) -> Result<Vec<Keyframe>> {
    // 先解码首批帧，从而 `cache.w()/h()` 已确定（维度在首次解码时填入）。
    cache.advance_to(FORWARD)?;
    let w = cache.w();
    let h = cache.h();
    run_state_machine(cache, w, h, params, "")
}

/// 状态机本体：对 `cache` 逐帧筛选（按需 `advance_to` 流式解码），输出关键帧。
#[allow(unused_assignments)]
fn run_state_machine(
    cache: &mut FrameCache,
    w: usize,
    h: usize,
    p: &Params,
    video_label: &str,
) -> Result<Vec<Keyframe>> {
    let dl = p.dl;
    let ddl = dl / 2;
    let ddl1_ofset = ddl - 1; // = 2
    let ddl2_ofset = 2 * ddl - 1; // = 5
    let size = w * h;

    if !video_label.is_empty() {
        eprintln!("subtitle-finder: 解码 {} 帧，{}x{}", cache.len(), w, h);
    }

    // 存储图像（对应 C++ 的状态变量）。
    let mut im_int_s = vec![0u8; size]; // ImIntS
    let mut im_int_sp = vec![0u8; size]; // ImIntSP
    let mut im_ne_s = vec![0u8; size]; // ImNES
    let mut im_ne_sp = vec![0u8; size]; // ImNESP
    let mut im_fs = vec![0u8; size * 3]; // ImFS（保存的 BGR）
    let mut im_fsp = vec![0u8; size * 3]; // ImFSP
    let mut im_y_s = vec![0u16; size]; // ImYS
    let mut im_y_sp = vec![0u16; size]; // ImYSP
    let mut prev_im_ne = vec![0u8; size]; // prevImNE

    let mut bf: i32 = -2;
    let mut ef: i32 = -2;
    let mut et: i64 = -2;
    let mut pbf: i32 = -2;
    let mut bt: i64 = -2;
    let mut pbt: i64 = -2;
    let mut pet: i64 = -2;
    let mut finded_prev: i32 = 0;
    let mut cmp_prev: i32 = 0;
    let mut found_sub: i32 = 0;

    let mut fn_: i32 = 0;
    let mut fn_start: i32 = 0;
    let mut prev_pos: i64 = -2;

    // 保存关键帧。
    let mut keyframes: Vec<Keyframe> = Vec::new();
    let mut save_keyframe = |im_fs: &[u8], mask: &[u8], start_ms: i64, end_ms: i64| {
        let arr = flat_bgr_to_array3(im_fs, w, h);
        let mask_arr = ndarray::Array2::from_shape_vec((h, w), mask.to_vec()).expect("mask 尺寸错误");
        keyframes.push(Keyframe {
            start_ms: start_ms.max(0) as u64,
            end_ms: end_ms.max(0) as u64,
            frame: arr,
            mask: mask_arr,
        });
    };

    // 检测阶段：找字幕起始。
    'outer: loop {
        // 流式推进：确保 [fn_start, fn_start+FORWARD] 已解码（fn_start 单调推进）。
        cache.advance_to(fn_start + FORWARD)?;
        // 内部搜索循环：仅当未找到字幕时运行（C++ `while(found_sub == 0)`）。
        if found_sub == 0 {
            loop {
                // 流式推进：检测内循环会连续推进 fn_start，需每次确保窗口覆盖
                // [fn_start, fn_start+FORWARD]，否则 fn_start 涨过 FORWARD 后
                // get_frame 越界会提前 break 'outer（长视频空字幕段会触发）。
                cache.advance_to(fn_start + FORWARD)?;
                // 推进 fn_start 的 ddl 步。
                // C++ 中先并行解码 fn_start+ddl1_ofset 与 fn_start+ddl2_ofset 帧。
                let f1 = match get_frame(&cache, fn_start + ddl1_ofset as i32) {
                    Ok(f) => f.clone(),
                    Err(_) => break 'outer, // 帧越界 → 结束
                };
                let bln1 = f1.has_text;
                let mut bln = false;
                if bln1 {
                    let f2 = match get_frame(&cache, fn_start + ddl2_ofset as i32) {
                        Ok(f) => f.clone(),
                        Err(_) => break 'outer,
                    };
                    let bln2 = f2.has_text;
                    if bln2 {
                        // ImInt = ImForward[fn_start+ddl1_ofset] ∩ ImForward[fn_start+ddl2_ofset]
                        let mut im_int = f1.im.clone();
                        imgops::intersect_two_images_inplace(&mut im_int, &f2.im, 0u8);
                        // ImYInt = ImYForward[0] ∩Y ImYForward[1]
                        let mut y_int = f1.y.clone();
                        imgops::intersect_y_images(&mut y_int, &f2.y, p.max_dl_down as i32, p.max_dl_up as i32);

                        bln = analyse_image_flat(&im_int, Some(&y_int), w, h, p);
                        if bln {
                            // 中间帧 [ddl1_ofset+1, ddl2_ofset-1]。
                            for i in (ddl1_ofset + 1)..=(ddl2_ofset - 1) {
                                let fi = match get_frame(&cache, fn_start + i as i32) {
                                    Ok(f) => f,
                                    Err(_) => break 'outer,
                                };
                                imgops::intersect_two_images_inplace(&mut im_int, &fi.im, 0u8);
                                imgops::intersect_y_images(&mut y_int, &fi.y, p.max_dl_down as i32, p.max_dl_up as i32);
                            }
                            bln = analyse_image_flat(&im_int, Some(&y_int), w, h, p);
                        }
                    }
                }

                if bln {
                    found_sub = 1;
                    fn_ = fn_start;
                    break;
                } else {
                    if bln1 {
                        // 需要确认 bln2；这里简化：用 f2 是否可取得来判断。
                        let bln2 = get_frame(&cache, fn_start + ddl2_ofset as i32).is_ok();
                        fn_start += if bln2 { ddl as i32 } else { 2 * ddl as i32 };
                    } else {
                        fn_start += ddl as i32;
                    }
                    if fn_start >= cache.len() as i32 {
                        break 'outer;
                    }
                }
            }
        }

        if found_sub == 0 {
            break 'outer;
        }

        // 追踪阶段：fn_ 为字幕起始。
        let f0 = match get_frame(&cache, fn_) {
            Ok(f) => f,
            Err(_) => {
                // EOF：保存当前进行中的字幕段。
                if bf != -2 {
                    let last = cache.len().saturating_sub(1) as i32;
                    if let Some(f) = try_frame(&cache, last) {
                        et = f.pos;
                    }
                    if last - bf + 1 >= p.dl as i32 {
                        let mut im_int = im_int_s.clone();
                        let mut im_y = im_y_s.clone();
                        if filter::analize_for_sub_presence(&im_ne_s, &mut im_int, &mut im_y, w, h, p) == 1 {
                            save_keyframe(&im_fs, &im_int, bt, et);
                        }
                    }
                    bf = -2;
                }
                break 'outer;
            }
        };
        prev_pos = if fn_ > 0 { get_frame(&cache, fn_ - 1)?.pos } else { -1 };
        let cur_pos = f0.pos;

        // bln = GetIntersectImages(fn)：ImInt = intersect(fn..fn+DL-1)。
        let (im_int, y_int, mut bln) = match get_intersect_images(&cache, fn_ as usize) {
            Some(v) => v,
            None => {
                // EOF：保存当前进行中的字幕段。
                if bf != -2 {
                    let last = cache.len().saturating_sub(1) as i32;
                    if let Some(f) = try_frame(&cache, last) {
                        et = f.pos;
                    }
                    if last - bf + 1 >= p.dl as i32 {
                        let mut im_int = im_int_s.clone();
                        let mut im_y = im_y_s.clone();
                        if filter::analize_for_sub_presence(&im_ne_s, &mut im_int, &mut im_y, w, h, p) == 1 {
                            save_keyframe(&im_fs, &im_int, bt, et);
                        }
                    }
                    bf = -2;
                }
                break 'outer; // 帧越界 → 结束
            }
        };

        // fn == bf → 记录当前为字幕段存储。
        if fn_ == bf {
            im_int_s = im_int.clone();
            im_ne_s = f0.ne.clone();
            im_y_s = y_int.clone();
            im_fs = f0.bgr.clone();
        }

        if fn_ > ef {
            if bln && cur_pos != prev_pos {
                if bf == -2 {
                    bf = fn_;
                    ef = bf;
                    bt = cur_pos;
                    im_int_s = im_int.clone();
                    im_ne_s = f0.ne.clone();
                    im_y_s = y_int.clone();
                    im_fs = f0.bgr.clone();
                } else {
                    // CompareTwoSubsOptimal(ImIntS, &ImYS, ImNES, prevImNE, ImInt, &ImYInt, ImNE)
                    bln = compare::compare_two_subs_optimal(
                        &im_int_s, Some(&im_y_s), &im_ne_s, Some(&prev_im_ne),
                        &im_int, Some(&y_int), &f0.ne, w, h, 0, w as i32 - 1, p,
                    );
                    if !bln {
                        trace!(fn_, bf, "追踪: 判定字幕内容变化");
                    }
                    if bln && (fn_ - bf + 1 == 3) {
                        im_fs = f0.bgr.clone();
                        im_ne_s = f0.ne.clone();
                        im_int_s = im_int.clone();
                        im_y_s = y_int.clone();
                    }
                    if !bln {
                        // bln == 0 → 字幕内容变化。
                        if finded_prev == 1 {
                            cmp_prev = compare::compare_two_subs_optimal(
                                &im_int_sp, Some(&im_y_sp), &im_ne_sp, Some(&im_ne_sp),
                                &im_int_s, Some(&im_y_s), &im_ne_s, w, h, 0, w as i32 - 1, p,
                            ) as i32;
                            if cmp_prev == 0 {
                                // 保存前一段。
                                if filter::analize_for_sub_presence(&im_ne_sp, &mut im_int_sp, &im_y_sp, w, h, p) == 1 {
                                    save_keyframe(&im_fsp, &im_int_sp, pbt, pet);
                                }
                                pbf = bf;
                                pbt = bt;
                            }
                        } else {
                            pbf = bf;
                            pbt = bt;
                        }

                        let mut pef = fn_ - 1;
                        let mut new_pet = cur_pos - 1;

                        let mut offset = 0usize;
                        if fn_ > bf + 1 {
                            offset = match find_offset_for_new_sub(
                                &cache, fn_ as usize, &im_int_s, &im_y_s, &im_ne_s, &prev_im_ne,
                            ) {
                                Some(o) => o,
                                None => break 'outer,
                            };
                            pef = fn_ + offset as i32 - 1;
                            let f_off = match try_frame(&cache, fn_ + offset as i32) {
                                Some(f) => f,
                                None => break 'outer,
                            };
                            new_pet = f_off.pos - 1;
                        }
                        pet = new_pet;

                        if pef - pbf + 1 >= dl as i32 {
                            if !((finded_prev == 1) && (cmp_prev == 1)) {
                                im_int_sp = im_int_s.clone();
                                im_fsp = im_fs.clone();
                                im_ne_sp = im_ne_s.clone();
                                im_y_sp = im_y_s.clone();
                            }
                            finded_prev = 1;
                        } else {
                            finded_prev = 0;
                        }

                        bf = fn_ + offset as i32;
                        ef = bf;
                        bt = f0.pos + offset as i64 * 1000 / 30;
                        let f_off = match try_frame(&cache, fn_ + offset as i32) {
                            Some(f) => f,
                            None => break 'outer,
                        };
                        im_ne_s = f_off.ne.clone();
                        im_fs = f_off.bgr.clone();
                        if offset == 0 {
                            im_int_s = im_int.clone();
                            im_y_s = y_int.clone();
                        } else {
                            im_int_s = f_off.im.clone();
                            im_y_s = f_off.y.iter().map(|&v| v as u16 + 255).collect();
                        }
                    } else {
                        // bln != 0 → 内容一致，扩展 YS。
                        imgops::intersect_y_images(&mut im_y_s, &f0.y, p.max_dl_down as i32, p.max_dl_up as i32);
                    }
                }
            } else if (bln == false && cur_pos != prev_pos) || (bln == true && cur_pos == prev_pos) {
                if finded_prev == 1 {
                    bln = compare::compare_two_subs_optimal(
                        &im_int_sp, Some(&im_y_sp), &im_ne_sp, Some(&im_ne_sp),
                        &im_int_s, Some(&im_y_s), &im_ne_s, w, h, 0, w as i32 - 1, p,
                    );
                    if bln {
                        bf = pbf;
                        ef = bf;
                        bt = pbt;
                        finded_prev = 0;
                    }
                }
                if bf != -2 {
                    if cur_pos != prev_pos {
                        // 逐个 offset 比较，找字幕结束。
                        let mut offset = 0usize;
                        let mut p_prev_ne = prev_im_ne.clone();
                        for off in 0..(dl - 1) {
                            let f_off = match try_frame(&cache, fn_ + off as i32) {
                                Some(f) => f,
                                None => {
                                    offset = off;
                                    break;
                                }
                            };
                            bln = compare::compare_two_subs_optimal(
                                &im_int_s, Some(&im_y_s), &im_ne_s, Some(&p_prev_ne),
                                &im_int_s, Some(&im_y_s), &f_off.ne,
                                w, h, 0, w as i32 - 1, p,
                            );
                            if !bln {
                                // 交集重试。
                                let mut ne_ff = f_off.ne.clone();
                                imgops::intersect_two_images_inplace(&mut ne_ff, &im_ne_s, 0u8);
                                bln = compare::compare_two_subs_optimal(
                                    &im_int_s, Some(&im_y_s), &im_ne_s, Some(&p_prev_ne),
                                    &im_int_s, Some(&im_y_s), &ne_ff, w, h, 0, w as i32 - 1, p,
                                );
                            }
                            if !bln {
                                offset = off;
                                break;
                            }
                            p_prev_ne = f_off.ne.clone();
                        }
                        ef = fn_ + offset as i32 - 1;
                        match try_frame(&cache, fn_ + offset as i32) {
                            Some(f) => et = f.pos - 1,
                            None => et = cur_pos,
                        }
                    } else {
                        ef = fn_ - 1;
                        et = cur_pos;
                    }

                    if ef - bf + 1 < dl as i32 {
                        if finded_prev == 1 {
                            bln = compare::compare_two_subs_optimal(
                                &im_int_s, Some(&im_y_s), &im_ne_sp, Some(&im_ne_sp),
                                &im_int_s, Some(&im_y_s), &im_ne_s, w, h, 0, w as i32 - 1, p,
                            );
                            if !bln {
                                let mut ne_sf = im_ne_s.clone();
                                imgops::intersect_two_images_inplace(&mut ne_sf, &im_ne_sp, 0u8);
                                bln = compare::compare_two_subs_optimal(
                                    &im_int_s, Some(&im_y_s), &im_ne_sp, Some(&im_ne_sp),
                                    &im_int_s, Some(&im_y_s), &ne_sf, w, h, 0, w as i32 - 1, p,
                                );
                                if !bln {
                                    let mut ne_spf = im_ne_sp.clone();
                                    imgops::intersect_two_images_inplace(&mut ne_spf, &im_ne_s, 0u8);
                                    bln = compare::compare_two_subs_optimal(
                                        &im_int_s, Some(&im_y_s), &ne_spf, Some(&ne_spf),
                                        &im_int_s, Some(&im_y_s), &im_ne_s, w, h, 0, w as i32 - 1, p,
                                    );
                                }
                            }
                            if bln {
                                bf = pbf;
                                bt = pbt;
                            }
                        }
                    }

                    if finded_prev == 1 && bf != pbf {
                        if filter::analize_for_sub_presence(&im_ne_sp, &mut im_int_sp, &im_y_sp, w, h, p) == 1 {
                            save_keyframe(&im_fsp, &im_int_sp, pbt, pet);
                        }
                    }

                    if ef - bf + 1 >= dl as i32 {
                        if bf != pbf {
                            if filter::analize_for_sub_presence(&im_ne_s, &mut im_int_s, &im_y_s, w, h, p) == 1 {
                                save_keyframe(&im_fs, &im_int_s, bt, et);
                            }
                        } else {
                            if filter::analize_for_sub_presence(&im_ne_sp, &mut im_int_sp, &im_y_sp, w, h, p) == 1 {
                                save_keyframe(&im_fsp, &im_int_sp, bt, et);
                            }
                        }
                    }
                }

                finded_prev = 0;
                bf = -2;

                if fn_ > ef {
                    if fn_ - fn_start >= dl as i32 {
                        found_sub = 0;
                        fn_start = fn_;
                    }
                }
            }
        }

        if found_sub != 0 {
            prev_im_ne = get_frame(&cache, fn_)?.ne.clone();
            fn_ += 1;
            // 流式推进：确保 [fn_+1, fn_+FORWARD] 已解码（追踪阶段 fn_ 单调递增）。
            cache.advance_to(fn_ + FORWARD)?;
            if fn_ >= cache.len() as i32 {
                // EOF：保存当前进行中的字幕段（否则末尾段丢失）。
                if bf != -2 {
                    let last = cache.len().saturating_sub(1) as i32;
                    if let Some(f) = try_frame(&cache, last) {
                        et = f.pos;
                    }
                    if last - bf + 1 >= p.dl as i32 {
                        let mut im_int = im_int_s.clone();
                        let mut im_y = im_y_s.clone();
                        if filter::analize_for_sub_presence(&im_ne_s, &mut im_int, &mut im_y, w, h, p) == 1 {
                            save_keyframe(&im_fs, &im_int, bt, et);
                        }
                    }
                    bf = -2;
                }
                break;
            }
        }
    }

    // 循环结束后的收尾：若仍有 finded_prev 段，保存（对齐 C++ 末尾段）。
    if finded_prev == 1 {
        if filter::analize_for_sub_presence(&im_ne_sp, &mut im_int_sp, &im_y_sp, w, h, p) == 1 {
            save_keyframe(&im_fsp, &im_int_sp, pbt, pet);
        }
    }

    Ok(keyframes)
}

/// flat BGR → `Array3`（H×W×3）。
fn flat_bgr_to_array3(bgr: &[u8], w: usize, h: usize) -> ndarray::Array3<u8> {
    let mut arr = ndarray::Array3::<u8>::zeros((h, w, 3));
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            arr[[y, x, 0]] = bgr[i];
            arr[[y, x, 1]] = bgr[i + 1];
            arr[[y, x, 2]] = bgr[i + 2];
        }
    }
    arr
}
