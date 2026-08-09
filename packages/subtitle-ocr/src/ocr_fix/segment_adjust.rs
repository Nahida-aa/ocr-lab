//! 字幕段置信度调整参数。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `OcrSegmentAdjustArgsSchema`：
//! - `iso_threshold_ms`：单帧孤立惩罚的参考时间（ms），在该时长内无同文帧则视为完全孤立；默认 1500。
//! - `adjust_y_weight`：Y 偏移在调整置信度中的权重（0~1）；默认 0.8。
//! - `adjust_iso_weight`：孤立程度在调整置信度中的权重（0~1）；默认 0.2。
//! - `adjust_y_factor`：Y 偏移惩罚归一化系数 `偏移量 / (videoHeight × adjust_y_factor)`，越小越严格；默认 0.08。
//!
//! 注：`videoHeight` 不属于该 schema（仅出现在 `adjust_y_factor` 的说明里），是调整函数
//! 的外部输入，不在此 struct 建模。
//!
//! 同模块还导出应用调整后的字幕段类型 [`OcrSegmentWithAdjusted`]（在 [`OcrSegment`]
//! 基础上补充 `adjusted_text_confidence` / `y_penalty` / `iso_penalty` 三个可选字段）。

use crate::OcrSegment;
use serde::Deserialize;
use serde::Serialize;

/// 字幕段置信度调整参数（对齐 LocalDub `OcrSegmentAdjustArgsSchema`）。
///
/// 四个字段都用 `Option` 保留「可省略」语义（对齐 zod 的 `.default(...)`），
/// 通过 [`OcrSegmentAdjustArgs::iso_threshold_ms`] / [`OcrSegmentAdjustArgs::adjust_y_weight`] /
/// [`OcrSegmentAdjustArgs::adjust_iso_weight`] / [`OcrSegmentAdjustArgs::adjust_y_factor`]
/// 取值即自动补默认。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct OcrSegmentAdjustArgs {
    /// 单帧孤立惩罚的参考时间（ms）：在该时长内无同文帧则视为完全孤立。默认 1500。
    #[serde(default = "default_iso_threshold_ms")]
    pub iso_threshold_ms: Option<u64>,
    /// Y 偏移在调整置信度中的权重（0~1）。默认 0.8。
    #[serde(default = "default_adjust_y_weight")]
    pub adjust_y_weight: Option<f32>,
    /// 孤立程度在调整置信度中的权重（0~1）。默认 0.2。
    #[serde(default = "default_adjust_iso_weight")]
    pub adjust_iso_weight: Option<f32>,
    /// Y 偏移惩罚归一化系数：偏移量 / (videoHeight × adjust_y_factor)，越小越严格。默认 0.08。
    #[serde(default = "default_adjust_y_factor")]
    pub adjust_y_factor: Option<f32>,
}

fn default_iso_threshold_ms() -> Option<u64> {
    Some(1500)
}

fn default_adjust_y_weight() -> Option<f32> {
    Some(0.8)
}

fn default_adjust_iso_weight() -> Option<f32> {
    Some(0.2)
}

fn default_adjust_y_factor() -> Option<f32> {
    Some(0.08)
}

impl OcrSegmentAdjustArgs {
    /// 解析实际生效的 `iso_threshold_ms`：省略时为默认 1500。
    pub fn iso_threshold_ms(&self) -> u64 {
        self.iso_threshold_ms.unwrap_or(1500)
    }

    /// 解析实际生效的 `adjust_y_weight`：省略时为默认 0.8。
    pub fn adjust_y_weight(&self) -> f32 {
        self.adjust_y_weight.unwrap_or(0.8)
    }

    /// 解析实际生效的 `adjust_iso_weight`：省略时为默认 0.2。
    pub fn adjust_iso_weight(&self) -> f32 {
        self.adjust_iso_weight.unwrap_or(0.2)
    }

    /// 解析实际生效的 `adjust_y_factor`：省略时为默认 0.08。
    pub fn adjust_y_factor(&self) -> f32 {
        self.adjust_y_factor.unwrap_or(0.08)
    }
}

/// 应用置信度调整后的字幕段（对齐 LocalDub `OcrSegmentWithAdjusted`）。
///
/// TS 用 `extends OcrSegment` 继承全部字段；Rust 无类型继承，用 `#[serde(flatten)]`
/// 内嵌 [`OcrSegment`]，序列化后与 TS 字段平铺一致。三个 `?` 字段（TS 可选）对应
/// `Option`，并以 `skip_serializing_if` 在输出时省略 `None`，与「可选字段缺省」语义对齐：
/// - `adjusted_text_confidence`：经 Y 偏移惩罚与孤立惩罚合成后的最终置信度；
/// - `y_penalty`：Y 偏移惩罚分量（0~1，越大偏移越严重）；
/// - `iso_penalty`：孤立程度惩罚分量（0~1，越大越孤立）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrSegmentWithAdjusted {
    #[serde(flatten)]
    pub base: OcrSegment,
    /// 调整后的最终文本置信度（经 Y 偏移惩罚 + 孤立惩罚合成）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjusted_text_confidence: Option<f32>,
    /// Y 偏移惩罚分量（0~1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_penalty: Option<f32>,
    /// 孤立程度惩罚分量（0~1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso_penalty: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let a = OcrSegmentAdjustArgs::default();
        assert_eq!(a.iso_threshold_ms(), 1500);
        assert!((a.adjust_y_weight() - 0.8).abs() < 1e-6);
        assert!((a.adjust_iso_weight() - 0.2).abs() < 1e-6);
        assert!((a.adjust_y_factor() - 0.08).abs() < 1e-6);
    }

    #[test]
    fn deserialize_omitted_uses_defaults() {
        let a: OcrSegmentAdjustArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(a.iso_threshold_ms(), 1500);
        assert!((a.adjust_y_weight() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn deserialize_overrides_defaults() {
        let a: OcrSegmentAdjustArgs =
            serde_json::from_str(r#"{"iso_threshold_ms":2000,"adjust_y_weight":0.5}"#).unwrap();
        assert_eq!(a.iso_threshold_ms(), 2000);
        assert!((a.adjust_y_weight() - 0.5).abs() < 1e-6);
        // 未提供的字段仍取默认。
        assert!((a.adjust_iso_weight() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn ocr_segment_with_adjusted_serializes_flattened_with_optional_omitted() {
        // 验证 OcrSegmentWithAdjusted 序列化后字段平铺（extends OcrSegment 语义），
        // 且三个调整字段为 None 时不出现在 JSON 中。
        let seg = OcrSegmentWithAdjusted {
            base: crate::OcrSegment {
                base: crate::SubtitlingSegment {
                    text: "你好".into(),
                    start_ms: 100,
                    end_ms: 200,
                },
                y_range: Some([10.0, 30.0]),
                text_confidence: 0.9,
                frame_count: Some(2),
                frames: None,
            },
            adjusted_text_confidence: None,
            y_penalty: None,
            iso_penalty: None,
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["text"], "你好");
        assert_eq!(v["start_ms"], 100);
        assert_eq!(v["end_ms"], 200);
        assert_eq!(v["text_confidence"], 0.9);
        assert!(v.get("adjusted_text_confidence").is_none(), "可选字段 None 应省略");
        assert!(v.get("y_penalty").is_none());
        assert!(v.get("iso_penalty").is_none());
    }

    #[test]
    fn ocr_segment_with_adjusted_serializes_with_optional_present() {
        let seg = OcrSegmentWithAdjusted {
            base: crate::OcrSegment {
                base: crate::SubtitlingSegment {
                    text: "你好".into(),
                    start_ms: 100,
                    end_ms: 200,
                },
                y_range: None,
                text_confidence: 0.9,
                frame_count: None,
                frames: None,
            },
            adjusted_text_confidence: Some(0.7),
            y_penalty: Some(0.1),
            iso_penalty: Some(0.2),
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["adjusted_text_confidence"], 0.7);
        assert_eq!(v["y_penalty"], 0.1);
        assert_eq!(v["iso_penalty"], 0.2);
    }
}
