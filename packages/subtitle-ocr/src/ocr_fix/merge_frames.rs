//! 多帧合并（相邻帧去重 / 子串合并）的参数。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `MergeFramesArgsSchema`：
//! - `is_merge_substring`：是否把互为子串的相邻文本合并；省略时默认 `false`。
//! - `dedup_edit_distance`：`dedupOverlap` 的编辑距离阈值，edit_distance ≤ 此值则合并；
//!   省略时默认 `1`。

use crate::SubtitlingSegment;
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
}
