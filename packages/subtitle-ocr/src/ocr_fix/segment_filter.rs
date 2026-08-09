//! 字幕段置信度过滤。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `ocrSegmentFilter`：从合并/调整后的
//! 字幕段出发，按置信度阈值 `textScore` 过滤；段若带 `adjusted_text_confidence`（Y 偏移 +
//! 孤立惩罚合成后的置信度）则优先用它，否则退回 `text_confidence`。低于阈值者丢弃，`textScore`
//! 为 0 时不过滤。
//!
//! 注：TS 入参是 `(OcrSegment | OcrSegmentWithAdjust)[]` 联合数组（两者字段可互读）。本库
//! `OcrSegment` 无 `adjusted_text_confidence` 字段，故入参统一为 `&[OcrSegmentWithAdjust]`；
//! 纯 `OcrSegment` 调用方先包成 `OcrSegmentWithAdjust`（惩罚字段置 `None`，即退回 `text_confidence`）
//! 即可，等价 TS 联合语义。

use crate::OcrSegmentWithAdjust;

/// 过滤结果：保留的段 + 被丢弃的数量。
#[derive(Clone, Debug)]
pub struct OcrSegmentFilterResult {
    /// 通过阈值的段（保持原顺序与类型）。
    pub segments: Vec<OcrSegmentWithAdjust>,
    /// 被 `textScore` 丢弃的段数量。
    pub dropped: usize,
}

/// 按置信度过滤字幕段（对齐 LocalDub `ocrSegmentFilter`）。
///
/// - `textScore` ≤ 0（含 0）视为不过滤，原样返回全部段、`dropped = 0`。
/// - 每个段取置信度优先级：`adjusted_text_confidence`（若 `Some`）→ 否则 `text_confidence`；
///   该置信度 ≥ `textScore` 才保留（TS 里 `undefined` 也保留——本库 `text_confidence` 必填，
///   仅当 `adjusted_text_confidence` 为 `None` 时退回必填的 `text_confidence`，不存在 undefined 情况）。
pub fn ocr_segment_filter(
    segments: &[OcrSegmentWithAdjust],
    text_score: f32,
) -> OcrSegmentFilterResult {
    if text_score <= 0.0 {
        return OcrSegmentFilterResult {
            segments: segments.to_vec(),
            dropped: 0,
        };
    }

    let filtered: Vec<OcrSegmentWithAdjust> = segments
        .iter()
        .filter(|s| {
            // 优先 adjusted_text_confidence，否则退回 text_confidence（必填）。
            let conf = s
                .adjusted_text_confidence
                .unwrap_or(s.base.text_confidence);
            conf >= text_score
        })
        .cloned()
        .collect();

    let dropped = segments.len() - filtered.len();
    OcrSegmentFilterResult {
        segments: filtered,
        dropped,
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
    fn no_filter_when_text_score_zero() {
        let segs = vec![adj("低", 0, 100, 0.1), adj("高", 0, 100, 0.9)];
        let out = ocr_segment_filter(&segs, 0.0);
        assert_eq!(out.segments.len(), 2);
        assert_eq!(out.dropped, 0);
    }

    #[test]
    fn drops_below_threshold_using_text_confidence() {
        let segs = vec![adj("低", 0, 100, 0.3), adj("高", 0, 100, 0.9)];
        let out = ocr_segment_filter(&segs, 0.5);
        assert_eq!(out.segments.len(), 1);
        assert_eq!(out.segments[0].base.base.text, "高");
        assert_eq!(out.dropped, 1);
    }

    #[test]
    fn prefers_adjusted_confidence_when_present() {
        // 段 text_confidence=0.9 但 adjusted=0.2（被惩罚压低）→ 用 adjusted 判定，应被丢弃。
        let segs = vec![adj_with("被惩罚", 0.9, 0.2), adj_with("正常", 0.9, 0.8)];
        let out = ocr_segment_filter(&segs, 0.5);
        assert_eq!(out.segments.len(), 1);
        assert_eq!(out.segments[0].base.base.text, "正常");
        assert_eq!(out.dropped, 1);
    }

    #[test]
    fn keeps_segment_at_exact_threshold() {
        let segs = vec![adj("临界", 0, 100, 0.5)];
        let out = ocr_segment_filter(&segs, 0.5);
        assert_eq!(out.segments.len(), 1, ">= 阈值应保留");
        assert_eq!(out.dropped, 0);
    }
}
