//! ocr_fix：对 OCR 结果的修正 / 统计后处理（行对齐、离群剔除、y 统计等）。
//!
//! 子模块：
//! - [`stats`]：字幕框纵向左统计（`compute_box_y_stats` / `YStats`）。
//! - [`box_adjusted`]：行对齐后的框调整参数（`BoxAdjustedArgs` / `build_ocr_frames_box_adjust` /
//!   `get_ocr_frames_box_filtered`）。
//! - [`merge_frames`]：多帧合并参数（`MergeFramesArgs`）与字幕段类型（`OcrSegment` / `SegmentFrame`）。
//! - [`segment_adjust`]：字幕段置信度调整参数（`OcrSegmentAdjustArgs`）。
//! - [`subtitling`]：字幕结果类型（`SubtitlingSegment`）。

pub(crate) mod box_adjusted;
pub(crate) mod merge_frames;
pub(crate) mod segment_adjust;
pub(crate) mod stats;
pub(crate) mod subtitling;
