//! ocr_post：对 OCR 结果的修正 / 统计后处理（行对齐、离群剔除、y 统计等）。
//!
//! 子模块：
//! - [`stats`]：字幕框纵向左统计（`compute_box_y_stats` / `YStats`）。
//! - [`box_adjusted`]：行对齐后的框调整参数（`BoxAdjustedArgs` / `ocr_frames_adjust_box` /
//!   `get_ocr_frames_box_filtered`）。
//! - [`merge_frames`]：多帧合并参数（`MergeFramesArgs`）与字幕段类型（`OcrSegment` / `SegmentFrame`）。
//! - [`segment_adjust`]：字幕段置信度调整参数（`OcrSegmentAdjustArgs`）。
//! - [`segment_filter`]：按置信度过滤字幕段（`ocr_segment_filter`）。
//! - [`subtitling`]：字幕结果类型（`SubtitlingSegment`）。

pub(crate) mod box_adjust;
pub(crate) mod box_filter;
pub(crate) mod merge_frames;
pub(crate) mod segment_adjust;
pub(crate) mod segment_filter;
pub(crate) mod stats;
pub(crate) mod subtitling;
