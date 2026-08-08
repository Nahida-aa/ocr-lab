//! ocr_fix：对 OCR 结果的修正 / 统计后处理（行对齐、离群剔除、y 统计等）。
//!
//! 子模块：
//! - [`stats`]：字幕框纵向左统计（`compute_box_y_stats` / `YStats`）。
//! - [`adjusted`]：行对齐后的框调整参数（`BoxAdjustedArgs`）。

pub(crate) mod adjusted;
pub(crate) mod stats;
