//! 多帧合并（相邻帧去重 / 子串合并）的参数。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `MergeFramesArgsSchema`：
//! - `is_merge_substring`：是否把互为子串的相邻文本合并；省略时默认 `false`。
//! - `dedup_edit_distance`：`dedupOverlap` 的编辑距离阈值，edit_distance ≤ 此值则合并；
//!   省略时默认 `1`。

use crate::{FrameResult, SubtitlingSegment};
use serde::Deserialize;
use serde::Serialize;

/// 多帧合并参数（对齐 LocalDub `MergeFramesArgsSchema`）。
///
/// 两个字段都用 `Option` 保留「可省略」语义（对齐 zod 的 `.optional()` + `.default(...)`），
/// 通过 [`MergeFramesArgs::is_merge_substring`] / [`MergeFramesArgs::dedup_edit_distance`]
/// 取值即自动补默认。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MergeFramesArgs {
    /// 是否合并互为子串的相邻文本。默认 `false`。
    #[serde(default = "default_is_merge_substring")]
    pub is_merge_substring: Option<bool>,
    /// dedupOverlap 的编辑距离阈值：edit_distance ≤ 此值则合并。默认 `1`。
    #[serde(default = "default_dedup_edit_distance")]
    pub dedup_edit_distance: Option<u32>,
}

fn default_is_merge_substring() -> Option<bool> {
    Some(false)
}

fn default_dedup_edit_distance() -> Option<u32> {
    Some(1)
}

impl MergeFramesArgs {
    /// 解析实际生效的 `is_merge_substring`：省略时为默认 `false`。
    pub fn is_merge_substring(&self) -> bool {
        self.is_merge_substring.unwrap_or(false)
    }

    /// 解析实际生效的 `dedup_edit_distance`：省略时为默认 `1`。
    pub fn dedup_edit_distance(&self) -> u32 {
        self.dedup_edit_distance.unwrap_or(1)
    }
}

/// 组成字幕段（[`OcrSegment`]）的单个帧明细（对齐 LocalDub `SegmentFrame`）。
///
/// `timestamp` 默认即毫秒（对齐 [`crate::FrameResult::timestamp`] 语义）；`text_confidence`
/// 为文本置信度（f32，与 [`crate::OcrBoxResult::text_confidence`] 一致）。
#[derive(Clone, Debug, Serialize)]
pub struct SegmentFrame {
    /// 帧文本。
    pub text: String,
    /// 帧时刻（毫秒）。
    pub timestamp: u64,
    /// 文本置信度。
    pub text_confidence: f32,
}

/// 一条字幕段（对齐 LocalDub `OcrSegment`）。
///
/// TS 用 `extends SubtitlingSegment` 继承 `text` / `start_ms` / `end_ms`；Rust 无类型继承，
/// 用 `#[serde(flatten)]` 内嵌 [`SubtitlingSegment`]，序列化后与 TS 字段平铺一致。
/// 带 `?` 的字段（TS 可选）对应 `Option`，并以 `skip_serializing_if` 在输出时省略 `None`，
/// 与「可选字段缺省」的语义对齐。
#[derive(Clone, Debug, Serialize)]
pub struct OcrSegment {
    #[serde(flatten)]
    pub base: SubtitlingSegment,
    /// 字幕带纵向值域 `[min_y, max_y]`（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_range: Option<[f32; 2]>,
    /// 字幕文本置信度（必填）。
    pub text_confidence: f32,
    /// 该段聚合的帧数（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_count: Option<u32>,
    /// 组成该段的各帧明细（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<SegmentFrame>>,
}

/// 归一化文本：去掉所有空白字符（对齐 LocalDub `utils.ts` 的 `normalize`，
/// TS 用 `s.replace(/\s+/g, "")`）。用于字幕文本的比对 / 子串合并判断，
/// 消除换行、空格、全/半角空白差异。返回新字符串，不改原输入。
pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 平均置信度：输入为空时返回 `0.0`（对齐 LocalDub `utils.ts` 的 `avgConfidence`）。
///
/// `confidences` 为各文本置信度（f32）；空切片返回 0 而非 panic / NaN。
pub fn avg_confidence(confidences: &[f32]) -> f32 {
    if confidences.is_empty() {
        0.0
    } else {
        confidences.iter().sum::<f32>() / confidences.len() as f32
    }
}

/// 判断 `a` 是否为 `b` 的子串（双向：较短者被较长者包含即算）。
///
/// 对齐 LocalDub `utils.ts` 的 `isSubstringOf`：
/// - 任一为空、或两串等长 ⇒ 返回 `false`（等长不算子串，避免自身匹配）；
/// - 否则较短串是否被较长串 `contains`。
///
/// 通常配合 [`normalize`] 先去空白再比对。
pub fn is_substring_of(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() || a.len() == b.len() {
        return false;
    }
    if a.len() < b.len() {
        b.contains(a)
    } else {
        a.contains(b)
    }
}

/// 合并两个（可选）置信度为均值；任一为 `None` 时返回另一个（再无则 `0.0`）。
///
/// 对齐 LocalDub `utils.ts` 的 `mergeConfidence`：参数均为可选 number，
/// 仅有一个有值时取该值，两者都有时取平均。
pub fn merge_confidence(a: Option<f32>, b: Option<f32>) -> f32 {
    match (a, b) {
        (Some(x), Some(y)) => (x + y) / 2.0,
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => 0.0,
    }
}

/// 编辑距离（Levenshtein）：将一个字符串变成另一个所需的最少
/// 插入 / 删除 / 替换次数（对齐 LocalDub `utils.ts` 的 `edit_distance`）。
///
/// 按**字符**遍历（而非字节），以对齐 TS `a.length`（UTF-16 码元计数，BMP 字符每字 1 个）。
/// 若按字节遍历，CJK 每字 3 字节会使距离被放大 3 倍，与 TS 结果不符。
/// 例：`edit_distance("陆", "陆执巡") == 2`（插入「执」「巡」两次）。
pub fn edit_distance(a: &str, b: &str) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    // 滚动两行 DP，避免 (m+1)*(n+1) 矩阵分配；每行长为 n+1。
    let mut prev: Vec<u32> = (0..=n as u32).collect();
    let mut cur: Vec<u32> = vec![0; n + 1];
    for i in 1..=m {
        cur[0] = i as u32;
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

/// 两个值域 `[min, max]` 是否重叠（对齐 LocalDub `utils.ts` 的 `overlap`）。
///
/// 重叠条件：`a[0] < b[1] && b[0] < a[1]`；任一为 `None` 返回 `false`。
/// 用于判断相邻帧的时间 / y 区间是否相交（如去重时的区间碰撞检测）。
pub fn overlap(a: Option<[f32; 2]>, b: Option<[f32; 2]>) -> bool {
    match (a, b) {
        (Some([a0, a1]), Some([b0, b1])) => a0 < b1 && b0 < a1,
        _ => false,
    }
}

/// 把逐帧 [`FrameResult`] 合并成带时间轴的字幕段 [`OcrSegment`]（对齐 LocalDub
/// `utils.ts` 的 `base_merge_frames`）。
///
/// 状态机：空帧标记 gap → gap 内同文本恢复则合并、否则 flush；文本变更则 flush 旧段开新段；
/// 循环结束 flush 最后一段。每段聚合所有组成帧的置信度均值、记录 `frame_count` 与各帧明细。
///
/// 注：TS 版签名含 `args: MergeFramesArgs`，但函数体内并未使用（去重/子串合并的阈值逻辑
/// 尚未接入）。为保持签名一致这里仍接收 `&MergeFramesArgs`，以 `_args` 命名避免未用警告，
/// 待后续接入 `is_merge_substring` / `dedup_edit_distance` 时再启用。
///
/// 类型映射：`FrameResult.text_confidence` 为 `f64`，而 [`OcrSegment`] / [`SegmentFrame`]
/// 的置信度为 `f32`；此处按 `as f32` 收敛（置信度 ~0..1，f32 精度足够）。
pub fn base_merge_frames(
    frames: &[FrameResult],
    _args: &MergeFramesArgs,
) -> Vec<OcrSegment> {
    let mut segments: Vec<OcrSegment> = Vec::new();
    let mut current_text = String::new();
    let mut current_start: u64 = 0;
    let mut current_end: u64 = 0;
    let mut current_box_y: Option<[f32; 2]> = None;
    let mut gap_start: u64 = 0;
    let mut current_confidences: Vec<f32> = Vec::new();
    let mut current_frames: Vec<SegmentFrame> = Vec::new();

    // 把当前累积的段 flush 进 segments（消费 current_frames，随后由调用方重置）。
    let flush = |current_text: &str,
                     current_start: u64,
                     end_ms: u64,
                     current_box_y: Option<[f32; 2]>,
                     current_confidences: &[f32],
                     current_frames: Vec<SegmentFrame>,
                     segments: &mut Vec<OcrSegment>| {
        segments.push(OcrSegment {
            base: SubtitlingSegment {
                text: current_text.to_string(),
                start_ms: current_start,
                end_ms,
            },
            y_range: current_box_y,
            text_confidence: avg_confidence(current_confidences),
            frame_count: Some(current_confidences.len() as u32),
            frames: Some(current_frames),
        });
    };

    for f in frames {
        // ─── A: 空帧 → 标记 gap ───
        if f.text.is_empty() {
            if !current_text.is_empty() && gap_start == 0 {
                gap_start = f.timestamp;
            }
            continue;
        }
        // ─── B: gap 恢复检查（空帧后同 text 恢复）───
        if gap_start > 0 {
            let gap_ms = f.timestamp.saturating_sub(gap_start);
            if gap_ms <= 1500
                && (normalize(&f.text) == normalize(&current_text)
                    || is_substring_of(&f.text, &current_text)
                    || is_substring_of(&current_text, &f.text))
            {
                // B1: gap 恢复成功 → 合并回当前段
                current_confidences.push(f.text_confidence as f32);
                current_end = f.timestamp;
                gap_start = 0;
                continue;
            }
            // B2: gap 恢复失败 → flush 当前段，重置
            flush(
                &current_text,
                current_start,
                gap_start,
                current_box_y,
                &current_confidences,
                std::mem::take(&mut current_frames),
                &mut segments,
            );
            current_text.clear();
            current_start = 0;
            current_box_y = None;
            gap_start = 0;
            current_confidences.clear();
            // current_frames 已被 take 走，重新置空
            current_frames = Vec::new();
        }
        // ─── C: text 比较 ───
        if current_text.is_empty() || normalize(&f.text) != normalize(&current_text) {
            // C1: 不同 text → flush 旧段，开始新段
            if !current_text.is_empty() {
                flush(
                    &current_text,
                    current_start,
                    current_end,
                    current_box_y,
                    &current_confidences,
                    std::mem::take(&mut current_frames),
                    &mut segments,
                );
            }
            current_text = f.text.clone();
            current_start = f.timestamp;
            current_end = f.timestamp;
            current_box_y = Some(f.y_range);
            current_confidences = vec![f.text_confidence as f32];
            current_frames = vec![SegmentFrame {
                timestamp: f.timestamp,
                text: f.text.clone(),
                text_confidence: f.text_confidence as f32,
            }];
        } else {
            // C2: 同 text → 延续当前段
            current_confidences.push(f.text_confidence as f32);
            current_end = f.timestamp;
            current_frames.push(SegmentFrame {
                timestamp: f.timestamp,
                text: f.text.clone(),
                text_confidence: f.text_confidence as f32,
            });
        }
    }
    // ─── D: 循环结束 flush 最后一段 ───
    if !current_text.is_empty() {
        let last_ts = if gap_start > 0 { gap_start } else { current_end };
        flush(
            &current_text,
            current_start,
            last_ts,
            current_box_y,
            &current_confidences,
            std::mem::take(&mut current_frames),
            &mut segments,
        );
    }
    segments
}

/// second pass：合并相邻且互为子串、且 y 值域重叠的段（对齐 LocalDub `utils.ts`
/// 的 `mergeSubstringSegments`，处理 OCR 单字幻觉，如 `身` → `绝不起身`）。
///
/// 从后往前遍历：对每对相邻段 `(prev, cur)`，`overlap` 为 false 则跳过；否则若
/// `prev.text` 是 `cur.text` 子串 → 用 cur 的文本、prev 的起点、cur 的终点与 y 值域；
/// 若 `cur.text` 是 `prev.text` 子串 → 保留 prev 的文本与 y 值域；置信度取
/// [`merge_confidence`]、`frame_count` 相加（TS 的 `?? 1` 对应 `unwrap_or(1)`）。
/// 合并后删除 cur（TS `splice(i, 1)`）。
///
/// 合并出的段**不携带 `frames`**（TS 合并字面量未含 `frames` 键，序列化即丢弃），
/// 与 TS 丢弃组成帧明细的语义一致。反向遍历保证 `remove(i)` 不影响尚未处理的 `< i` 下标。
pub fn merge_substring_segments(segments: &[OcrSegment]) -> Vec<OcrSegment> {
    let mut out: Vec<OcrSegment> = segments.to_vec();
    let mut i = out.len();
    while i > 1 {
        i -= 1;
        // 先把需要比较/合并的标量读成自有值，结束对 out 的不可变借用后再改 out。
        let prev_text = out[i - 1].base.text.clone();
        let prev_y = out[i - 1].y_range;
        let prev_conf = out[i - 1].text_confidence;
        let prev_fc = out[i - 1].frame_count;
        let prev_start = out[i - 1].base.start_ms;

        let cur_text = out[i].base.text.clone();
        let cur_y = out[i].y_range;
        let cur_conf = out[i].text_confidence;
        let cur_fc = out[i].frame_count;
        let cur_end = out[i].base.end_ms;

        if !overlap(prev_y, cur_y) {
            continue;
        }
        let (merged_text, merged_y) = if is_substring_of(&prev_text, &cur_text) {
            (cur_text, cur_y)
        } else if is_substring_of(&cur_text, &prev_text) {
            (prev_text, prev_y)
        } else {
            continue;
        };
        let conf = merge_confidence(Some(prev_conf), Some(cur_conf));
        let fc = prev_fc.unwrap_or(1) + cur_fc.unwrap_or(1);
        out[i - 1] = OcrSegment {
            base: SubtitlingSegment {
                text: merged_text,
                start_ms: prev_start,
                end_ms: cur_end,
            },
            y_range: merged_y,
            text_confidence: conf,
            frame_count: Some(fc),
            frames: None,
        };
        out.remove(i);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_merge_substring_is_false() {
        assert!(!MergeFramesArgs::default().is_merge_substring());
    }

    #[test]
    fn default_dedup_edit_distance_is_one() {
        assert_eq!(MergeFramesArgs::default().dedup_edit_distance(), 1);
    }

    #[test]
    fn explicit_fields_override_default() {
        let a = MergeFramesArgs {
            is_merge_substring: Some(true),
            dedup_edit_distance: Some(3),
        };
        assert!(a.is_merge_substring());
        assert_eq!(a.dedup_edit_distance(), 3);
    }

    #[test]
    fn deserialize_omitted_fields_use_default() {
        let a: MergeFramesArgs = serde_json::from_str("{}").unwrap();
        assert!(!a.is_merge_substring());
        assert_eq!(a.dedup_edit_distance(), 1);
    }

    #[test]
    fn deserialize_partial_fields_use_default_for_omitted() {
        let a: MergeFramesArgs = serde_json::from_str(r#"{"is_merge_substring":true}"#).unwrap();
        assert!(a.is_merge_substring());
        assert_eq!(a.dedup_edit_distance(), 1);
    }

    #[test]
    fn normalize_strips_all_whitespace() {
        assert_eq!(normalize("a b\nc\t d"), "abcd");
        // 原输入不被修改（返回新字符串）。
        let s = "x y".to_string();
        assert_eq!(normalize(&s), "xy");
        assert_eq!(s, "x y");
    }

    #[test]
    fn avg_confidence_mean_and_empty() {
        assert_eq!(avg_confidence(&[0.9, 0.8, 0.7]), 0.8);
        assert_eq!(avg_confidence(&[]), 0.0);
    }

    #[test]
    fn ocr_segment_serializes_flattened_with_optional_omitted() {
        // 验证 OcrSegment 序列化后字段平铺（extends SubtitlingSegment 语义），
        // 且可选字段（y_range/frame_count/frames）为 None 时不出现在 JSON 中。
        let seg = OcrSegment {
            base: SubtitlingSegment {
                text: "hello".into(),
                start_ms: 100,
                end_ms: 200,
            },
            y_range: None,
            text_confidence: 0.9,
            frame_count: None,
            frames: None,
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["text"], "hello");
        assert_eq!(v["start_ms"], 100);
        assert_eq!(v["end_ms"], 200);
        assert_eq!(v["text_confidence"], 0.9);
        assert!(v.get("y_range").is_none(), "可选字段 None 应省略");
        assert!(v.get("frame_count").is_none());
        assert!(v.get("frames").is_none());
    }

    #[test]
    fn ocr_segment_serializes_with_optional_present() {
        let seg = OcrSegment {
            base: SubtitlingSegment {
                text: "hi".into(),
                start_ms: 0,
                end_ms: 500,
            },
            y_range: Some([10.0, 30.0]),
            text_confidence: 0.8,
            frame_count: Some(2),
            frames: Some(vec![SegmentFrame {
                text: "hi".into(),
                timestamp: 0,
                text_confidence: 0.8,
            }]),
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["y_range"][0], 10.0);
        assert_eq!(v["frame_count"], 2);
        assert_eq!(v["frames"][0]["timestamp"], 0);
        assert_eq!(v["frames"][0]["text_confidence"], 0.8);
    }

    #[test]
    fn is_substring_of_bidirectional() {
        // 短串被长串包含 ⇒ true（双向：a 短则 b 含 a，反之 a 含 b）。
        assert!(is_substring_of("abc", "xxabcxx"));
        assert!(is_substring_of("xxabcxx", "abc"));
        // 等长 ⇒ false（避免自身匹配）。
        assert!(!is_substring_of("abc", "abc"));
        // 任一为空 ⇒ false。
        assert!(!is_substring_of("", "abc"));
        assert!(!is_substring_of("abc", ""));
        // 互不包含 ⇒ false。
        assert!(!is_substring_of("abc", "def"));
    }

    #[test]
    fn merge_confidence_combines() {
        // 两者都有 ⇒ 均值（f32 有舍入，用近似比较）。
        assert!((merge_confidence(Some(0.8), Some(0.6)) - 0.7).abs() < 1e-5);
        // 仅一个 ⇒ 取该值。
        assert_eq!(merge_confidence(Some(0.9), None), 0.9);
        assert_eq!(merge_confidence(None, Some(0.4)), 0.4);
        // 都为 None ⇒ 0.0。
        assert_eq!(merge_confidence(None, None), 0.0);
    }

    #[test]
    fn edit_distance_matches_ts_examples() {
        // 陆 → 陆执巡：插入「执」「巡」= 2 次操作。
        assert_eq!(edit_distance("陆", "陆执巡"), 2);
        // 陆 → 这其中是不是有什么误会：替换「陆」并插入其余 10 字 = 11 次操作
        // （该串实际为 11 个 BMP 字符；TS 注释里写的 9 为注释笔误）。
        assert_eq!(edit_distance("陆", "这其中是不是有什么误会"), 11);
        // 相同串距离为 0；单字符相等为 0。
        assert_eq!(edit_distance("abc", "abc"), 0);
        // 单字符替换 = 1。
        assert_eq!(edit_distance("a", "b"), 1);
    }

    #[test]
    fn overlap_detects_range_intersection() {
        // [0,10) 与 [5,15) 相交。
        assert!(overlap(Some([0.0, 10.0]), Some([5.0, 15.0])));
        // [0,10) 与 [10,20) 相邻不相交（严格小于）。
        assert!(!overlap(Some([0.0, 10.0]), Some([10.0, 20.0])));
        // 任一为 None ⇒ false。
        assert!(!overlap(None, Some([0.0, 10.0])));
    }

    /// 构造一个带文本与时刻的帧（其余字段占位）。
    fn frame(text: &str, ts: u64, conf: f64) -> FrameResult {
        FrameResult {
            text: text.into(),
            text_confidence: conf,
            boxes: vec![],
            x_range: [0.0, 10.0],
            y_range: [10.0, 30.0],
            timestamp: ts,
        }
    }

    #[test]
    fn base_merge_frames_groups_same_text_and_splits_on_change() {
        // 两帧同文本 → 合并成一段；第三帧不同文本 → 另起一段。
        let frames = vec![
            frame("你好", 0, 0.9),
            frame("你好", 500, 0.8),
            frame("世界", 1000, 0.7),
        ];
        let segs = base_merge_frames(&frames, &MergeFramesArgs::default());
        assert_eq!(segs.len(), 2, "同文本合并、异文本分段");
        assert_eq!(segs[0].base.text, "你好");
        assert_eq!(segs[0].base.start_ms, 0);
        assert_eq!(segs[0].base.end_ms, 500);
        assert_eq!(segs[0].frame_count, Some(2));
        // 置信度为两帧均值 (0.9+0.8)/2=0.85。
        assert!((segs[0].text_confidence - 0.85).abs() < 1e-5);
        assert_eq!(segs[1].base.text, "世界");
        assert_eq!(segs[1].base.start_ms, 1000);
    }

    #[test]
    fn base_merge_frames_recovers_gap_when_same_text() {
        // 中间空帧（gap）后同文本、间隔 < 1500ms → 合并回当前段（不分段）。
        let frames = vec![
            frame("字幕", 0, 0.9),
            frame("", 500, 0.0), // 空帧标记 gap
            frame("字幕", 800, 0.8),
        ];
        let segs = base_merge_frames(&frames, &MergeFramesArgs::default());
        assert_eq!(segs.len(), 1, "gap 内同文本恢复应合并为一段");
        assert_eq!(segs[0].frame_count, Some(2));
        assert_eq!(segs[0].base.end_ms, 800);
    }

    #[test]
    fn base_merge_frames_flushes_on_gap_then_different_text() {
        // 空帧后文本不同 → flush 当前段、开新段（两段）。
        let frames = vec![
            frame("甲", 0, 0.9),
            frame("", 500, 0.0),
            frame("乙", 900, 0.8), // 间隔 400ms 但文本不同
        ];
        let segs = base_merge_frames(&frames, &MergeFramesArgs::default());
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].base.text, "甲");
        assert_eq!(segs[0].base.end_ms, 500); // gap 处截断
        assert_eq!(segs[1].base.text, "乙");
    }

    #[test]
    fn base_merge_frames_empty_input_yields_no_segments() {
        let segs = base_merge_frames(&[], &MergeFramesArgs::default());
        assert!(segs.is_empty());
    }

    /// 构造一个字幕段（其余字段占位）。
    fn segment(text: &str, start: u64, end: u64, y: [f32; 2], conf: f32) -> OcrSegment {
        OcrSegment {
            base: SubtitlingSegment {
                text: text.into(),
                start_ms: start,
                end_ms: end,
            },
            y_range: Some(y),
            text_confidence: conf,
            frame_count: Some(1),
            frames: None,
        }
    }

    #[test]
    fn merge_substring_segments_merges_overlapping_substring() {
        // prev「身」是 cur「绝不起身」的子串、y 重叠 → 合并为「绝不起身」，
        // 起点取 prev、终点取 cur。
        let segs = vec![
            segment("身", 0, 500, [10.0, 30.0], 0.9),
            segment("绝不起身", 500, 1000, [10.0, 30.0], 0.8),
        ];
        let merged = merge_substring_segments(&segs);
        assert_eq!(merged.len(), 1, "互为子串且 y 重叠应合并");
        assert_eq!(merged[0].base.text, "绝不起身");
        assert_eq!(merged[0].base.start_ms, 0);
        assert_eq!(merged[0].base.end_ms, 1000);
        // 置信度取均值 (0.9+0.8)/2 = 0.85。
        assert!((merged[0].text_confidence - 0.85).abs() < 1e-5);
        assert_eq!(merged[0].frame_count, Some(2));
    }

    #[test]
    fn merge_substring_segments_skips_when_y_not_overlap() {
        // 文本互为子串但 y 不重叠 → 不合并。
        let segs = vec![
            segment("身", 0, 500, [10.0, 30.0], 0.9),
            segment("绝不起身", 500, 1000, [200.0, 220.0], 0.8),
        ];
        let merged = merge_substring_segments(&segs);
        assert_eq!(merged.len(), 2, "y 不重叠应保留两段");
    }

    #[test]
    fn merge_substring_segments_skips_when_not_substring() {
        // 文本互不包含、y 重叠 → 不合并。
        let segs = vec![
            segment("你好", 0, 500, [10.0, 30.0], 0.9),
            segment("世界", 500, 1000, [10.0, 30.0], 0.8),
        ];
        let merged = merge_substring_segments(&segs);
        assert_eq!(merged.len(), 2, "非子串应保留两段");
    }
}
