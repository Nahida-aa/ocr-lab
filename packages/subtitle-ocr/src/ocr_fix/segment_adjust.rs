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
}
