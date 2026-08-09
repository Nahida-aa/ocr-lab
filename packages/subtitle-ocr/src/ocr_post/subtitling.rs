//! 字幕（subtitling）结果类型。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `SubtitlingSegment`：一段字幕的
//! 文本与时间跨度。

use serde::Serialize;

/// 一段字幕（对齐 LocalDub `SubtitlingSegment`）。
///
/// `start_ms` / `end_ms` 为该段字幕的时间跨度（毫秒），对应 [`crate::FrameResult::timestamp`]
/// 的时间语义，用 `u64`。`text` 为字幕文本。
#[derive(Clone, Debug, Serialize)]
pub struct SubtitlingSegment {
    /// 字幕文本。
    pub text: String,
    /// 起始时间（毫秒）。
    pub start_ms: u64,
    /// 结束时间（毫秒）。
    pub end_ms: u64,
}
