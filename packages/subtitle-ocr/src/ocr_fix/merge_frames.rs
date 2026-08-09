//! 多帧合并（相邻帧去重 / 子串合并）的参数。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `MergeFramesArgsSchema`：
//! - `is_merge_substring`：是否把互为子串的相邻文本合并；省略时默认 `false`。
//! - `dedup_edit_distance`：`dedupOverlap` 的编辑距离阈值，edit_distance ≤ 此值则合并；
//!   省略时默认 `1`。

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
        let a: MergeFramesArgs = serde_json::from_str(r#"{"isMergeSubstring":true}"#).unwrap();
        assert!(a.is_merge_substring());
        assert_eq!(a.dedup_edit_distance(), 1);
    }
}
