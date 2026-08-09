//! 字幕段置信度过滤。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `ocrSegmentFilter` / `ocrSegmentFilterWithMeta`：
//! 从合并/调整后的字幕段出发，按置信度阈值 `text_confidence_threshold` 过滤；段若带
//! `adjusted_text_confidence`（Y 偏移 + 孤立惩罚合成后的置信度）则优先用它，否则退回
//! `text_confidence`。低于阈值者丢弃，`text_confidence_threshold` 为 0 时不过滤。
//!
//! 注：TS 入参是 `(OcrSegment | OcrSegmentWithAdjust)[]` 联合数组（两者字段可互读）。本库
//! `OcrSegment` 无 `adjusted_text_confidence` 字段，故入参统一为 `&[OcrSegmentWithAdjust]`；
//! 纯 `OcrSegment` 调用方先包成 `OcrSegmentWithAdjust`（惩罚字段置 `None`，即退回 `text_confidence`）
//! 即可，等价 TS 联合语义。

use crate::OcrSegmentWithAdjust;
use serde::Serialize;

/// 过滤结果（对齐 LocalDub `OcrSegmentFilterResult`）。
///
/// 由 [`ocr_segment_filter_with_meta`] 产出：`meta` 为过滤统计，`result` 为过滤后的字幕段
/// 与全文拼接（与 `mergeFrames` 输出形状一致）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrSegmentFilterResult {
    /// 过滤统计（段数 / 阈值 / 丢弃数）。
    pub meta: OcrSegmentFilterMeta,
    /// 过滤后的字幕段与全文。
    pub result: OcrSegmentFilterData,
}

/// [`OcrSegmentFilterResult::meta`]：过滤统计。
#[derive(Clone, Debug, Serialize)]
pub struct OcrSegmentFilterMeta {
    /// 过滤后保留的段数。
    pub segment_count: usize,
    /// 本次使用的置信度阈值。
    pub text_confidence_threshold: f32,
    /// 被丢弃的段数。
    pub dropped: usize,
}

/// [`OcrSegmentFilterResult::result`]：过滤后的字幕段与全文。
#[derive(Clone, Debug, Serialize)]
pub struct OcrSegmentFilterData {
    /// 全文：各段文本按空格拼接。
    pub text: String,
    /// 过滤后保留的字幕段。
    pub segments: Vec<OcrSegmentWithAdjust>,
}

/// 按置信度过滤字幕段（对齐 LocalDub `ocrSegmentFilter`）。
///
/// - `text_confidence_threshold` ≤ 0（含 0）视为不过滤，原样返回全部段。
/// - 每个段取置信度优先级：`adjusted_text_confidence`（若 `Some`）→ 否则 `text_confidence`；
///   该置信度 ≥ `text_confidence_threshold` 才保留（TS 里 `undefined` 也保留——本库 `text_confidence`
///   必填，仅当 `adjusted_text_confidence` 为 `None` 时退回必填的 `text_confidence`，不存在 undefined 情况）。
///
/// 返回过滤后的段数组（不携带 `dropped` 统计；需要统计请用 [`ocr_segment_filter_with_meta`]）。
pub fn ocr_segment_filter(
    segments: &[OcrSegmentWithAdjust],
    text_confidence_threshold: f32,
) -> Vec<OcrSegmentWithAdjust> {
    if text_confidence_threshold <= 0.0 {
        return segments.to_vec();
    }

    segments
        .iter()
        .filter(|s| {
            // 优先 adjusted_text_confidence，否则退回 text_confidence（必填）。
            let conf = s.adjusted_text_confidence.unwrap_or(s.base.text_confidence);
            conf >= text_confidence_threshold
        })
        .cloned()
        .collect()
}

/// 按置信度过滤并返回带统计的结果（对齐 LocalDub `ocrSegmentFilterWithMeta`）。
///
/// 委托 [`ocr_segment_filter`] 做过滤，再封装 `meta`（段数 / 阈值 / 丢弃数）与 `result`
/// （全文 `text` + 过滤后的 `segments`），形状与 `mergeFrames` 输出一致。
pub fn ocr_segment_filter_with_meta(
    segments: &[OcrSegmentWithAdjust],
    text_confidence_threshold: f32,
) -> OcrSegmentFilterResult {
    let filtered = ocr_segment_filter(segments, text_confidence_threshold);
    let dropped = segments.len() - filtered.len();
    let text = filtered
        .iter()
        .map(|s| s.base.base.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    OcrSegmentFilterResult {
        meta: OcrSegmentFilterMeta {
            segment_count: filtered.len(),
            text_confidence_threshold,
            dropped,
        },
        result: OcrSegmentFilterData {
            text,
            segments: filtered,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OcrSegment, SubtitlingSegment};

    /// 构造一个 OcrSegmentWithAdjust（惩罚字段默认 None，退回 text_confidence）。
    fn adj(text: &str, start: u64, end: u64, conf: f32) -> OcrSegmentWithAdjust {
        OcrSegmentWithAdjust {
            base: OcrSegment {
                base: SubtitlingSegment {
                    text: text.into(),
                    start_ms: start,
                    end_ms: end,
                },
                y_range: Some([10.0, 30.0]),
                text_confidence: conf,
                frame_count: Some(1),
                frames: None,
            },
            adjusted_text_confidence: None,
            y_penalty: None,
            iso_penalty: None,
        }
    }

    /// 带 adjusted_text_confidence 的段。
    fn adj_with(text: &str, conf: f32, adjusted: f32) -> OcrSegmentWithAdjust {
        let mut s = adj(text, 0, 100, conf);
        s.adjusted_text_confidence = Some(adjusted);
        s
    }

    #[test]
    fn no_filter_when_threshold_zero() {
        let segs = vec![adj("低", 0, 100, 0.1), adj("高", 0, 100, 0.9)];
        let out = ocr_segment_filter(&segs, 0.0);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn drops_below_threshold_using_text_confidence() {
        let segs = vec![adj("低", 0, 100, 0.3), adj("高", 0, 100, 0.9)];
        let out = ocr_segment_filter(&segs, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].base.base.text, "高");
    }

    #[test]
    fn prefers_adjusted_confidence_when_present() {
        // 段 text_confidence=0.9 但 adjusted=0.2（被惩罚压低）→ 用 adjusted 判定，应被丢弃。
        let segs = vec![adj_with("被惩罚", 0.9, 0.2), adj_with("正常", 0.9, 0.8)];
        let out = ocr_segment_filter(&segs, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].base.base.text, "正常");
    }

    #[test]
    fn keeps_segment_at_exact_threshold() {
        let segs = vec![adj("临界", 0, 100, 0.5)];
        let out = ocr_segment_filter(&segs, 0.5);
        assert_eq!(out.len(), 1, ">= 阈值应保留");
    }

    #[test]
    fn with_meta_reports_counts_and_text() {
        let segs = vec![
            adj_with("你好", 0.9, 0.8),
            adj("世界", 0, 100, 0.3), // 低于 0.5 被丢弃
        ];
        let out = ocr_segment_filter_with_meta(&segs, 0.5);
        assert_eq!(out.meta.segment_count, 1);
        assert_eq!(out.meta.dropped, 1);
        assert!((out.meta.text_confidence_threshold - 0.5).abs() < 1e-6);
        assert_eq!(out.result.text, "你好");
        assert_eq!(out.result.segments.len(), 1);
    }

    #[test]
    fn with_meta_no_filter_when_threshold_zero() {
        let segs = vec![adj("a", 0, 100, 0.1), adj("b", 0, 100, 0.9)];
        let out = ocr_segment_filter_with_meta(&segs, 0.0);
        assert_eq!(out.meta.segment_count, 2);
        assert_eq!(out.meta.dropped, 0);
        assert_eq!(out.result.text, "a b");
    }
}
