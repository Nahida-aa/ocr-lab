//! 字幕 OCR 帧合并层。
//!
//! 把「逐帧（带时间戳）的 [`OcrImgResult`]」聚合成「带时间轴的字幕段 [`Segment`]」。
//!
//! 设计边界：[`subtitle_ocr`] 库只吃一张图、不知道图片来源（视频帧/截图/扫描件），
//! 因而只产出**无时间**的 [`OcrImgResult`]。**带时间的封装是本层的职责**——只有
//! 「知道视频结构（帧率/帧索引/文件名时间戳）」的上游才该在这里把单图结果包成
//! [`FrameResult`]（`OcrImgResult` + `timestamp_ms`）并调用 [`merge_frames`]。
//!
//! 该层与具体 OCR 引擎、视频来源完全解耦，可被多个项目复用（这也是独立于
//! `subtitle-ocr` 单独成 crate 的原因）。

use rapidocr_ort::OcrBoxResult;
use serde::Serialize;

/// 单图聚合结果（无时间）：把一张图里识别出的多框聚合成一条文本 + 值域 + 明细。
///
/// 这是「纯感知」产物——`subtitle-ocr` 只吃一张图、不知道图片来源（视频帧/截图/
/// 扫描件），故不携带任何时间/帧信息。带时间的封装（`FrameResult`）在本层负责。
///
/// 定义在本 crate（而非 `subtitle-ocr`），以便依赖单向：
/// `subtitle-ocr` → `subtitle-ocr-merge` → `rapidocr-ort`。`subtitle-ocr` 通过
/// `pub use subtitle_ocr_merge::OcrImgResult` 把它重新导出，对外仍像「subtitle-ocr 提供」。
#[derive(Clone, Debug)]
pub struct OcrImgResult {
    /// 该图识别文本（多行按出现顺序拼接，用空格分隔）。
    pub text: String,
    /// 该图最高置信度（取各框 `text_confidence` 最大）。
    pub confidence: f64,
    /// 该图所有识别区域明细（每行文本/框/score，含坐标还原）。
    pub boxes: Vec<OcrBoxResult>,
    /// 横向值域 `[min_x, max_x]`（像素坐标），无字幕时为 `[0,0]`。
    pub x_range: [f32; 2],
    /// 纵向值域 `[min_y, max_y]`（像素坐标），无字幕时为 `[0,0]`。
    pub y_range: [f32; 2],
}

/// 把一图识别出的多框聚合成单图结果（无时间，纯感知后处理）。
///
/// 过滤 / 坐标还原 / NMS / 排序由上游（`subtitle-ocr` 的 `ocr_image`）完成，这里只做
/// 「多行拼接成一条文本 + 取最高置信度 + 算几何值域」。不接收时间戳（本层不知道
/// 图片来源/帧信息）。
pub fn aggregate_img(lines: &[OcrBoxResult]) -> OcrImgResult {
    let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    let text = text.join(" ");
    let confidence = lines
        .iter()
        .map(|l| l.text_confidence as f64)
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
    OcrImgResult {
        text,
        confidence,
        boxes: lines.to_vec(),
        x_range,
        y_range,
    }
}

/// 单帧结果（带时间戳）：`OcrImgResult` + 该帧的时间信息。
///
/// 字段平铺展开自 [`OcrImgResult`]，故访问 `fr.text` / `fr.boxes` 等与单图结果一致；
/// 仅多一个 `timestamp_ms`（毫秒）。由上游用 [`FrameResult::from((img, ts))`] 构造。
#[derive(Clone, Debug)]
pub struct FrameResult {
    /// 该帧识别文本（多行拼接）。
    pub text: String,
    /// 该帧最高置信度（取各框 `text_confidence` 最大）。
    pub confidence: f64,
    /// 该帧所有识别区域明细。
    pub boxes: Vec<OcrBoxResult>,
    /// 横向值域 `[min_x, max_x]`（像素坐标）。
    pub x_range: [f32; 2],
    /// 纵向值域 `[min_y, max_y]`（像素坐标）。
    pub y_range: [f32; 2],
    /// 时间戳（毫秒），由上游（帧序号 × 帧间隔 / 文件名解析）提供。
    pub timestamp_ms: u64,
}

impl From<(OcrImgResult, u64)> for FrameResult {
    fn from((img, timestamp_ms): (OcrImgResult, u64)) -> Self {
        Self {
            text: img.text,
            confidence: img.confidence,
            boxes: img.boxes,
            x_range: img.x_range,
            y_range: img.y_range,
            timestamp_ms,
        }
    }
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
pub fn texts_mergeable(a: &str, b: &str, args: &MergeArgs) -> bool {
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
}
