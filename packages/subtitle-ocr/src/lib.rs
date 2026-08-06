//! # subtitle-ocr
//!
//! 字幕 OCR 专用层，构建在 [`rapidocr_ort::OcrEngine`]（PP-OCR det/rec/cls）之上。
//!
//! 本包不做模型推理，只做「字幕场景」的专属逻辑，与 cpp 实现
//! （`packages/subtitle-ocr-cpp/ocr_pipeline.cpp`）对齐，便于 bench 公平对比：
//!
//! - **bottom_only ROI**：只送画面底部 40% 给引擎，提速（cpp 默认开启）。
//! - **subtitle_only y 过滤**：仅保留 y 中心落在底部比例区间的字幕框。
//! - **NMS 去重**：剔除被大框高度覆盖的重叠框（cpp `--no-nms` 可关）。
//! - **多帧合并 + 计时**：相邻帧同文本合并成带 `start/end` 的段（LocalDub
//!   `mergeFrames` 风格）。
//!
//! 计时口径与 cpp 一致：模型只加载一次，`--dir` 循环内仅对每帧累加推理耗时
//! （cpp 的 `total = det + post + rec`；rapidocr-ort 的 `detect` 把 det/rec 合成一次
//! 调用，故本包把整段 `detect` 计为 `detInferenceMs`，post/rec 记为 0，总和即 RTF 口径）。

use anyhow::Result;
use ndarray::{s, Array3};
use rapidocr_ort::{ModelProfile, OcrEngine};
use serde::Serialize;

// ===========================================================================
// 选项与结果类型
// ===========================================================================

/// 字幕 OCR 的行为开关（对齐 cpp 的 CLI 参数）。
#[derive(Clone, Debug)]
pub struct OcrOptions {
    /// 只裁底部 40% 送 OCR（cpp 默认 true）。
    pub bottom_only: bool,
    /// 仅保留 y 中心在画面底部比例区间的字幕框（cpp `--subtitle-only`）。
    pub subtitle_only: bool,
    /// 重叠框 NMS 去重（cpp 默认 true，`--no-nms` 关闭）。
    pub use_nms: bool,
    /// 识别置信度下限（cpp `text_score`，默认 0.5）。
    pub text_score: f32,
    /// 是否用 cpp 同款的透视矫正裁剪（warpPerspective）替代轴对齐包围盒。
    /// 配合 det 几何 minAreaRect 一起用（两者耦合）；默认 false。
    pub use_warp_crop: bool,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            bottom_only: true,
            subtitle_only: false,
            use_nms: true,
            text_score: 0.5,
            use_warp_crop: false,
        }
    }
}

/// 单个文字识别区域（`rapidocr_ort::OcrBox` 的 re-export）。
///
/// 原先是自定义 `FrameLine` 结构体，字段几乎与 rapidocr-ort 的 `OcrResult`
/// 相同（text/confidence/box_），仅多了 `y_center`（= `center[1]`）。后改为
/// 类型别名，并统一命名为 `OcrBox`（表示"一个识别区域/文本框"）。
/// ⚠️ 坐标语义：`ocr_image` 返回前会把 box/center 的 y 加回 `y_offset`，
/// 还原成原图坐标。
pub use rapidocr_ort::OcrBox;

/// 单帧聚合结果（供 `merge_frames` 消费）：每帧一条文本。
#[derive(Clone, Debug)]
pub struct FrameResult {
    /// 该帧识别文本（多行按出现顺序拼接，用空格分隔）。
    pub text: String,
    /// 该帧最高置信度（取各行最大）。
    pub confidence: f64,
    /// 该帧所有识别区域明细（每行文本/框/score，含坐标还原）。
    pub boxes: Vec<OcrBox>,
    /// 横向值域 `[min_x, max_x]`（像素坐标），无字幕时为 `[0,0]`。
    pub x_range: [f32; 2],
    /// 纵向值域 `[min_y, max_y]`（像素坐标），无字幕时为 `[0,0]`。
    pub y_range: [f32; 2],
    /// 时间戳（毫秒），由帧序号 × 帧间隔得到。
    pub timestamp_ms: u64,
}

/// 合并后的字幕段（对齐 LocalDub 的 Segment）。
#[derive(Clone, Debug, Serialize)]
pub struct Segment {
    /// 合并后的文本。
    pub text: String,
    /// 段起始时间（毫秒）= 首帧时间戳。
    pub start_ms: u64,
    /// 段结束时间（毫秒）= 末帧时间戳（单帧字幕 start==end，即零时长段）。
    pub end_ms: u64,
    /// 段平均置信度。
    pub confidence: f64,
    /// 段横向值域 `[min_x, max_x]`（像素坐标），取覆盖帧的最小/最大。
    pub x_range: [f32; 2],
    /// 段纵向值域 `[min_y, max_y]`（像素坐标），取覆盖帧的最小/最大。
    pub y_range: [f32; 2],
}

/// 帧合并参数（对齐 ocrMerge.ts 的可调项）。
#[derive(Clone, Debug)]
pub struct MergeArgs {
    /// 相邻段若间隔 ≤ 此值（毫秒）且文本接近则合并。
    pub merge_gap_ms: u64,
    /// 文本视为「接近」的 levenshtein 上限（字符数），超过则不再合并。
    pub merge_levenshtein: usize,
    /// 单帧文本被另一帧文本「包含」（前缀/后缀）即视为延续，忽略 levenshtein。
    pub allow_substring: bool,
}

impl Default for MergeArgs {
    fn default() -> Self {
        Self {
            merge_gap_ms: 500,
            merge_levenshtein: 2,
            allow_substring: true,
        }
    }
}

// ===========================================================================
// 引擎封装
// ===========================================================================

/// 字幕 OCR 引擎：持有 [`OcrEngine`] 与行为选项。
pub struct SubtitleOcr {
    engine: OcrEngine,
    opts: OcrOptions,
}

impl SubtitleOcr {
    /// 按模型套件构建（模型目录默认仓库根 `models/rapidocr`）。
    pub fn from_profile(
        profile: ModelProfile,
        model_dir: &std::path::Path,
        opts: OcrOptions,
    ) -> Result<Self> {
        let engine = OcrEngine::from_profile(profile, model_dir)?
            .with_warp_crop(opts.use_warp_crop);
        Ok(Self { engine, opts })
    }

    /// 对一帧 RGB 图像（H×W×3，0-255 u8）做字幕 OCR，返回排序后的识别行。
    ///
    /// 流程对齐 cpp `runOcr`：bottom_only ROI → subtitle_only y 过滤 → NMS。
    pub fn ocr_image(&mut self, rgb: &Array3<u8>) -> Result<Vec<OcrBox>> {
        let (h, _, _) = rgb.dim();
        let h = h as i64;

        // ---- 1. bottom_only：裁底部 40% 作为 ROI ----
        let y_offset = if self.opts.bottom_only {
            ((h as f32) * 0.6) as i64
        } else {
            0
        };
        let roi: Array3<u8> = if y_offset > 0 {
            rgb.slice(s![y_offset as usize.., .., ..]).to_owned()
        } else {
            rgb.clone()
        };

        // ---- 2. 引擎推理（det + rec + cls）----
        let results: Vec<OcrBox> = self.engine.detect(&roi)?;

        // ---- 3. 后处理：还原坐标 / y 过滤 / NMS / trim / 排序 ----
        let mut lines: Vec<OcrBox> = results
            .into_iter()
            .map(|mut r| {
                // ROI 坐标还原回原图：box 每点 y 与 center.y 都加 y_offset。
                if y_offset > 0 {
                    for p in &mut r.box_ {
                        p[1] += y_offset as f32;
                    }
                    r.center[1] += y_offset as f32;
                }
                r.text = r.text.trim().to_string();
                r
            })
            .filter(|l| {
                // subtitle_only：y 中心须落在画面底部 [0.85, 0.99]（cpp 比值口径）。
                if self.opts.subtitle_only {
                    let ratio = l.center[1] / (h as f32);
                    if !(0.85..=0.99).contains(&ratio) {
                        return false;
                    }
                }
                !l.text.is_empty() && l.confidence >= self.opts.text_score
            })
            .collect();

        if self.opts.use_nms && lines.len() > 1 {
            lines = nms(lines);
        }

        // 排序：先按 y 中心，差 ≤20px 再按 x 中心（cpp 的 TL/BR 排序等价）。
        lines.sort_by(|a, b| {
            let ya = a.center[1];
            let yb = b.center[1];
            if (ya - yb).abs() > 20.0 {
                ya.partial_cmp(&yb).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                let xa = a.box_[0][0];
                let xb = b.box_[0][0];
                xa.partial_cmp(&xb).unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        Ok(lines)
    }

    /// 同 [`ocr_image`]，但返回每帧推理耗时（毫秒）。
    ///
    /// rapidocr-ort 的 `detect` 把 det/rec 合成一次调用，无法单独计时；
    /// 故把整段 `detect` 计为 `det_ms`，`post_ms`/`rec_ms` 记 0，三者之和即
    /// cpp 的 `totalMs` 口径（post 在 rapidocr 内部极小，近似 0）。
    pub fn ocr_image_timed(&mut self, rgb: &Array3<u8>) -> Result<(Vec<OcrBox>, f64)> {
        let t0 = std::time::Instant::now();
        let lines = self.ocr_image(rgb)?;
        let det_ms = t0.elapsed().as_secs_f64() * 1000.0;
        Ok((lines, det_ms))
    }

    /// 把一帧的多条行聚合成 `FrameResult`（多行拼接、取最高置信度与纵向值域）。
    pub fn aggregate_frame(&self, lines: &[OcrBox], timestamp_ms: u64) -> FrameResult {
        let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        let text = text.join(" ");
        let confidence = lines
            .iter()
            .map(|l| l.confidence as f64)
            .fold(0.0f64, f64::max);
        // 聚合所有行的四点坐标，取 x / y 值域（无字幕 → [0,0]）。
        let mut x_range = [f32::INFINITY, f32::NEG_INFINITY];
        let mut y_range = [f32::INFINITY, f32::NEG_INFINITY];
        for l in lines {
            for p in &l.box_ {
                x_range[0] = x_range[0].min(p[0]);
                x_range[1] = x_range[1].max(p[0]);
                y_range[0] = y_range[0].min(p[1]);
                y_range[1] = y_range[1].max(p[1]);
            }
        }
        let (x_range, y_range) = if lines.is_empty() {
            ([0.0, 0.0], [0.0, 0.0])
        } else {
            (x_range, y_range)
        };
        FrameResult {
            text,
            confidence,
            boxes: lines.to_vec(),
            x_range,
            y_range,
            timestamp_ms,
        }
    }
}

// ===========================================================================
// NMS（复刻 cpp runOcr 的重叠框过滤）
// ===========================================================================

/// 按面积降序，剔除被已保留大框覆盖超过 70% 的小框（IoU 口径）。
fn nms(mut lines: Vec<OcrBox>) -> Vec<OcrBox> {
    // 计算外接框。
    struct B {
        idx: usize,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        area: f32,
    }
    let mut boxes: Vec<B> = lines
        .iter()
        .enumerate()
        .map(|(idx, l)| {
            let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
            for p in &l.box_ {
                x0 = x0.min(p[0]);
                x1 = x1.max(p[0]);
                y0 = y0.min(p[1]);
                y1 = y1.max(p[1]);
            }
            let area = (x1 - x0).max(1.0) * (y1 - y0).max(1.0);
            B { idx, x0, y0, x1, y1, area }
        })
        .collect();
    // 面积大的优先保留。
    boxes.sort_by(|a, b| b.area.partial_cmp(&a.area).unwrap_or(std::cmp::Ordering::Equal));

    let mut keep = vec![true; lines.len()];
    for i in 0..boxes.len() {
        if !keep[boxes[i].idx] {
            continue;
        }
        let a = &boxes[i];
        for j in (i + 1)..boxes.len() {
            if !keep[boxes[j].idx] {
                continue;
            }
            let b = &boxes[j];
            let ix0 = a.x0.max(b.x0);
            let iy0 = a.y0.max(b.y0);
            let ix1 = a.x1.min(b.x1);
            let iy1 = a.y1.min(b.y1);
            if ix1 <= ix0 || iy1 <= iy0 {
                continue;
            }
            let inter = (ix1 - ix0) * (iy1 - iy0);
            // 小框 b 被大框 a 覆盖比例。
            if inter / b.area > 0.7 {
                keep[boxes[j].idx] = false;
            }
        }
    }
    let mut out: Vec<OcrBox> = keep
        .iter()
        .zip(lines.drain(..))
        .filter(|(k, _)| **k)
        .map(|(_, l)| l)
        .collect();
    // 维持原顺序（按 y 排序在 ocr_image 末尾统一做；此处仅 NMS 过滤，先按输入序）。
    out.sort_by_key(|l| (l.center[1] * 1000.0) as i64);
    out
}

// ===========================================================================
// 帧合并（对齐 LocalDub ocrMerge.ts mergeFrames）
// ===========================================================================

/// 帧序号 → 时间戳（毫秒）：`index * 1000 / fps`。
pub fn frame_timestamp_ms(index: usize, fps: f64) -> u64 {
    if fps <= 0.0 {
        return 0;
    }
    ((index as f64) * 1000.0 / fps) as u64
}

/// 编辑距离（本包自带，避免反向依赖 bench 包）。
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut cur = vec![0usize; n + 1];
    for i in 1..=m {
        cur[0] = i;
        for j in 1..=n {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1]
            } else {
                prev[j].min(cur[j - 1]).min(prev[j - 1]) + 1
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

/// 两帧文本是否视作「同一字幕的延续」（合并条件）。
fn texts_mergeable(a: &str, b: &str, args: &MergeArgs) -> bool {
    if a == b {
        return true;
    }
    if args.allow_substring {
        // 一者包含另一者（前缀/后缀）即视为同一行不同识别完整度。
        if a.contains(b) || b.contains(a) {
            return true;
        }
    }
    levenshtein(a, b) <= args.merge_levenshtein
}

/// 把逐帧结果合并成带时间轴的字幕段。
///
/// 规则（对齐 LocalDub mergeFrames 空参数）：
/// - 维护一个「开放段」，首帧开段。
/// - 下一帧若文本可与当前段末帧合并、且时间戳间隔 ≤ `merge_gap_ms`，则并入
///   （文本取较长的，置信度取均值，纵向值域取并集，end 刷新为末帧）。
/// - 否则关闭当前段、开新段。
/// - 单帧字幕产生 start==end 的零时长段（与 cpp 行为一致，保留不补偿）。
pub fn merge_frames(frames: &[FrameResult], args: &MergeArgs) -> Vec<Segment> {
    if frames.is_empty() {
        return Vec::new();
    }
    let mut segments: Vec<Segment> = Vec::new();
    let mut cur: Option<Segment> = None;
    // 当前段已并入的帧数（含开段的那一帧），用于计算正确的算术均值。
    let mut cur_count: usize = 0;
    let mut cur_last_text: String = String::new();

    for f in frames {
        let start_new = match &cur {
            None => true,
            Some(c) => {
                let gap = f.timestamp_ms.saturating_sub(c.end_ms);
                if gap > args.merge_gap_ms {
                    true
                } else {
                    !texts_mergeable(&cur_last_text, &f.text, args)
                }
            }
        };

        if start_new {
            if let Some(c) = cur.take() {
                segments.push(c);
            }
            cur = Some(Segment {
                text: f.text.clone(),
                start_ms: f.timestamp_ms,
                end_ms: f.timestamp_ms,
                confidence: f.confidence,
                x_range: f.x_range,
                y_range: f.y_range,
            });
            cur_count = 1;
            cur_last_text = f.text.clone();
        } else if let Some(c) = cur.as_mut() {
            // 并入：文本取较长者，置信度取**所有帧的算术均值**（非逐对平均，
            // 逐对平均在有 >2 帧时会偏离真值），值域取并集，end 刷新。
            if f.text.chars().count() > c.text.chars().count() {
                c.text = f.text.clone();
            }
            cur_count += 1;
            c.confidence = (c.confidence * (cur_count - 1) as f64 + f.confidence)
                / cur_count as f64;
            c.end_ms = f.timestamp_ms;
            c.x_range = [c.x_range[0].min(f.x_range[0]), c.x_range[1].max(f.x_range[1])];
            c.y_range = [c.y_range[0].min(f.y_range[0]), c.y_range[1].max(f.y_range[1])];
            cur_last_text = f.text.clone();
        }
    }
    if let Some(c) = cur.take() {
        segments.push(c);
    }
    segments
}

// ===========================================================================
// 白盒测试（纯函数，无需 ONNX 模型）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一帧结果。
    fn fr(text: &str, ts: u64, conf: f64, y_range: Option<(f32, f32)>) -> FrameResult {
        // 测试里只给 y 值域；x 用固定占位。
        let y_range = y_range.unwrap_or((0.0, 0.0));
        FrameResult {
            text: text.to_string(),
            confidence: conf,
            boxes: Vec::new(),
            x_range: [0.0, 0.0],
            y_range: [y_range.0, y_range.1],
            timestamp_ms: ts,
        }
    }

    #[test]
    fn timestamp_math() {
        // fps=2 → 每帧 500ms。
        assert_eq!(frame_timestamp_ms(0, 2.0), 0);
        assert_eq!(frame_timestamp_ms(1, 2.0), 500);
        assert_eq!(frame_timestamp_ms(2, 2.0), 1000);
        // fps=4 → 每帧 250ms。
        assert_eq!(frame_timestamp_ms(3, 4.0), 750);
        // 非法 fps 回落 0，不 panic。
        assert_eq!(frame_timestamp_ms(5, 0.0), 0);
    }

    #[test]
    fn levenshtein_known() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("试炼那日", "试炼那日"), 0);
        // 单字符差异。
        assert_eq!(levenshtein("abc", "abd"), 1);
    }

    #[test]
    fn merge_empty() {
        assert!(merge_frames(&[], &MergeArgs::default()).is_empty());
    }

    #[test]
    fn merge_single_frame_is_zero_duration() {
        // 单帧字幕 → start==end（零时长段，与 cpp 一致）。
        let frames = vec![fr("你好", 1000, 0.9, Some((600.0, 650.0)))];
        let segs = merge_frames(&frames, &MergeArgs::default());
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start_ms, 1000);
        assert_eq!(segs[0].end_ms, 1000);
        assert_eq!(segs[0].text, "你好");
        assert_eq!(segs[0].y_range, [600.0, 650.0]);
    }

    #[test]
    fn merge_same_text_merges_across_frames() {
        // 3 帧同文本、间隔 500ms ≤ gap，应合并为一段 start=0 end=1000。
        let frames = vec![
            fr("试炼那日", 0, 0.9, Some((640.0, 686.0))),
            fr("试炼那日", 500, 0.95, Some((645.0, 687.0))),
            fr("试炼那日", 1000, 0.92, Some((646.0, 688.0))),
        ];
        let segs = merge_frames(&frames, &MergeArgs::default());
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs[0].end_ms, 1000);
        // 置信度取均值。
        assert!((segs[0].confidence - (0.9 + 0.95 + 0.92) / 3.0).abs() < 1e-9);
        // 纵向值域取并集。
        assert_eq!(segs[0].y_range, [640.0, 688.0]);
    }

    #[test]
    fn merge_gap_splits_segment() {
        // 间隔 1500ms > 默认 gap 500ms → 拆成两段。
        let frames = vec![
            fr("第一句", 0, 0.9, None),
            fr("第一句", 500, 0.9, None),
            fr("第一句", 2000, 0.9, None),
        ];
        let segs = merge_frames(&frames, &MergeArgs::default());
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].end_ms, 500);
        assert_eq!(segs[1].start_ms, 2000);
        assert_eq!(segs[1].end_ms, 2000);
    }

    #[test]
    fn merge_text_change_splits_segment() {
        // 文本差异超 levenshtein 阈值（"你好世界" vs "天气真好" lev=4 > 2）
        // → 即便间隔很小也拆段。
        let frames = vec![
            fr("你好世界", 0, 0.9, None),
            fr("天气真好", 500, 0.9, None),
        ];
        let segs = merge_frames(&frames, &MergeArgs::default());
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn merge_substring_continues() {
        // allow_substring：短文本被长文本包含即视为延续。
        let args = MergeArgs {
            allow_substring: true,
            ..Default::default()
        };
        let frames = vec![fr("你好", 0, 0.9, None), fr("你好世界", 500, 0.9, None)];
        let segs = merge_frames(&frames, &args);
        assert_eq!(segs.len(), 1);
        // 文本取较长者。
        assert_eq!(segs[0].text, "你好世界");
        assert_eq!(segs[0].end_ms, 500);
    }

    #[test]
    fn merge_substring_disabled_splits() {
        // 关闭 substring 后，"你好" vs "你好世界" levenshtein=2 ≤ 阈值 2，仍合并；
        // 但 "你好" vs "你好世界啊" levenshtein=3 > 2 → 拆段。
        let args = MergeArgs {
            allow_substring: false,
            merge_levenshtein: 2,
            ..Default::default()
        };
        let frames = vec![fr("你好", 0, 0.9, None), fr("你好世界啊", 500, 0.9, None)];
        let segs = merge_frames(&frames, &args);
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn texts_mergeable_predicate() {
        let args = &MergeArgs::default();
        assert!(texts_mergeable("abc", "abc", args));
        assert!(texts_mergeable("ab", "abc", args)); // substring
        assert!(texts_mergeable("abc", "abd", args)); // lev=1
        assert!(!texts_mergeable("abc", "xyz", args)); // lev=3 > 2
    }

    #[test]
    fn nms_removes_contained_small_box() {
        // 大框包含小框（覆盖 >70%）→ 小框被剔除。
        let big = OcrBox {
            text: "A".into(),
            confidence: 0.9,
            box_: [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]],
            score: 0.9,
            x_range: [0.0, 100.0],
            y_range: [0.0, 100.0],
            center: [50.0, 50.0],
        };
        let small = OcrBox {
            text: "B".into(),
            confidence: 0.8,
            box_: [[10.0, 10.0], [20.0, 10.0], [20.0, 20.0], [10.0, 20.0]],
            score: 0.8,
            x_range: [10.0, 20.0],
            y_range: [10.0, 20.0],
            center: [15.0, 15.0],
        };
        let out = nms(vec![big.clone(), small]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "A");
    }

    #[test]
    fn nms_keeps_disjoint_boxes() {
        let a = OcrBox {
            text: "A".into(),
            confidence: 0.9,
            box_: [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            score: 0.9,
            x_range: [0.0, 10.0],
            y_range: [0.0, 10.0],
            center: [5.0, 5.0],
        };
        let b = OcrBox {
            text: "B".into(),
            confidence: 0.9,
            box_: [[100.0, 100.0], [110.0, 100.0], [110.0, 110.0], [100.0, 110.0]],
            score: 0.9,
            x_range: [100.0, 110.0],
            y_range: [100.0, 110.0],
            center: [105.0, 105.0],
        };
        let out = nms(vec![a, b]);
        assert_eq!(out.len(), 2);
    }
}
