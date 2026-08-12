//! 字幕框离群过滤：逐帧剔除离群框、重聚合得到干净帧。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `get_ocr_frames_box_filtered`：
//! 在 [`crate::ocr_post::box_adjust`] 的行对齐 / 调整之后，按 `is_outlier` 标记过滤离群框。
//! 本模块只负责「过滤 + 重聚合」，不碰 box 调整参数（那些在 `box_adjust`）。

use crate::ocr_post::box_adjust::{
    FrameResultBoxWithAdjust, OcrBoxResultWithAdjust, OcrFramesBoxFilteredResult,
    OcrFramesBoxFilteredResultMeta,
};
use crate::ocr_util::aggregate_boxes;
use crate::{FrameResult, compute_box_y_stats};

/// 过滤离群框：逐帧剔除 `is_outlier` 的框后，重新聚合得到干净帧。
///
/// 对齐 LocalDub `get_ocr_frames_box_filtered`（返回 [`OcrFramesBoxFilteredResult`]）：
/// - 全部框都是离群 → 丢弃该帧；
/// - 无离群框 → 原帧转回 [`FrameResult`] 返回；
/// - 部分离群 → 用干净框调 [`aggregate_boxes`] 重聚合成新帧（text/confidence/x_range/
///   y_range/boxes 取自重聚结果，其余帧字段如 `timestamp` 保留原值）。
///
/// 返回的是干净 [`FrameResult`] 序列（`From` 投影已丢弃 adjust 附加字段），正好对应 TS
/// 用 `as FrameResult` / 重建后 adjust 元数据实际丢失的语义。最终包成
/// [`OcrFramesBoxFilteredResult`]，其 `meta.y_stats` 对**过滤后**的帧重新统计
/// （对齐 TS `computeBoxYStats(filteredFrames)`）。
pub fn ocr_frames_filter_box(frames: &[FrameResultBoxWithAdjust]) -> OcrFramesBoxFilteredResult {
    let frames: Vec<FrameResult> = frames
        .iter()
        .flat_map(|f| {
            let clean_boxes: Vec<&OcrBoxResultWithAdjust> =
                f.boxes.iter().filter(|b| !b.is_outlier).collect();
            if clean_boxes.is_empty() {
                return Vec::new(); // 全离群 → 丢帧
            }
            if clean_boxes.len() == f.boxes.len() {
                // 无离群 → 原帧转回 FrameResult
                return vec![f.clone().into()];
            }
            // 部分离群 → 干净框重聚合（`aggregate_boxes` 只聚合 `OcrBoxResult`，
            // 不携带 `timestamp`，需保留原帧的时刻）。
            let mut rebuilt_ocr = aggregate_boxes(
                &clean_boxes
                    .iter()
                    .map(|b| b.base.clone())
                    .collect::<Vec<_>>(),
            );
            rebuilt_ocr.timestamp = f.timestamp;
            vec![rebuilt_ocr]
        })
        .collect();
    OcrFramesBoxFilteredResult {
        meta: OcrFramesBoxFilteredResultMeta {
            y_stats: compute_box_y_stats(&frames),
            frame_count: frames.len(),
        },
        frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OcrBoxResult;
    use crate::ocr_post::box_adjust::OcrBoxResultWithAdjust;

    fn box_with(text: &str, y_range: [f32; 2], conf: f32) -> OcrBoxResult {
        OcrBoxResult {
            text: text.into(),
            text_confidence: conf,
            box_confidence: conf,
            bbox: [
                [0.0, y_range[0]],
                [10.0, y_range[0]],
                [10.0, y_range[1]],
                [0.0, y_range[1]],
            ],
            x_range: [0.0, 10.0],
            y_range,
            center: [5.0, (y_range[0] + y_range[1]) / 2.0],
        }
    }

    /// 构造一个调整后框（adjust 字段用给定值，便于测试 is_outlier 过滤）。
    fn adjust_box_with(
        text: &str,
        y_range: [f32; 2],
        conf: f32,
        is_outlier: bool,
    ) -> OcrBoxResultWithAdjust {
        OcrBoxResultWithAdjust {
            base: box_with(text, y_range, conf),
            y_center_offset_ratio: 0.0,
            x_center_offset_ratio: 0.0,
            height: y_range[1] - y_range[0],
            height_ratio: 1.0,
            y_penalty: 0.0,
            x_penalty: 0.0,
            height_penalty: 0.0,
            total_penalty: 0.0,
            is_outlier,
            adjusted_confidence: conf,
        }
    }

    /// 用一组调整后框构造一帧（强制 sameLine=false 的 y 不重叠，确保聚合不被合并）。
    fn adjust_frame(boxes: Vec<OcrBoxResultWithAdjust>, ts: u64) -> FrameResultBoxWithAdjust {
        FrameResultBoxWithAdjust {
            text: String::new(),
            text_confidence: 0.0,
            x_range: [0.0, 0.0],
            y_range: [0.0, 0.0],
            timestamp: ts,
            boxes,
        }
    }

    #[test]
    fn all_outlier_frame_is_dropped() {
        // 整帧框都是离群 → 过滤后该帧消失。
        let f = adjust_frame(
            vec![
                adjust_box_with("a", [400.0, 420.0], 0.9, true),
                adjust_box_with("b", [410.0, 430.0], 0.8, true),
            ],
            100,
        );
        let out = ocr_frames_filter_box(&[f]);
        assert!(out.frames.is_empty(), "全离群帧应被丢弃");
        assert_eq!(out.meta.frame_count, 0);
    }

    #[test]
    fn no_outlier_frame_passthrough() {
        // 无离群框 → 原帧转回 FrameResult 返回（含原 timestamp）。
        let f = adjust_frame(
            vec![
                adjust_box_with("a", [100.0, 120.0], 0.9, false),
                adjust_box_with("b", [200.0, 220.0], 0.8, false),
            ],
            12345,
        );
        let out = ocr_frames_filter_box(&[f]);
        assert_eq!(out.frames.len(), 1);
        assert_eq!(out.frames[0].timestamp, 12345);
        assert_eq!(out.frames[0].boxes.len(), 2);
        assert_eq!(out.meta.frame_count, 1);
    }

    #[test]
    fn partial_outlier_frame_rebuilt() {
        // 部分离群 → 干净框重聚合，离群框被剔除、新帧保留原 timestamp。
        // 两个干净框 y 不重叠（sameLine=false），聚合后 text 用换行连接，boxes 数量为 2。
        let f = adjust_frame(
            vec![
                adjust_box_with("a", [100.0, 120.0], 0.9, false),
                adjust_box_with("b", [200.0, 220.0], 0.8, false),
                adjust_box_with("c", [400.0, 420.0], 0.9, true), // 离群，剔除
            ],
            999,
        );
        let out = ocr_frames_filter_box(&[f]);
        assert_eq!(out.frames.len(), 1, "部分离群帧保留");
        assert_eq!(out.frames[0].timestamp, 999, "timestamp 保留原值");
        assert_eq!(out.frames[0].boxes.len(), 2, "离群框被剔除");
        assert_eq!(out.meta.frame_count, 1, "meta.frame_count 取过滤后帧数");
        // 输出为干净 FrameResult，框就是普通 OcrBoxResult（无 adjust 字段）。
        // meta.y_stats 应基于过滤后的帧重算：剩两个框 y=[100,120]/[200,220]。
        assert_eq!(out.meta.y_stats.median_height, 20.0);
    }
}
