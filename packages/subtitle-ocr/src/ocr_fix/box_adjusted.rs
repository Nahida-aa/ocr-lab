//! 字幕框调整（行对齐后的离群剔除 / 置信度调整）的参数与入口。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `BoxAdjustedArgsSchema` /
//! `build_ocr_frames_box_adjust`：`box_adjusted_threshold` 为触发 box 调整的置信度阈值
//! （低于此值的框进入调整流程），默认 0.5。

use crate::ocr_util::aggregate_boxes;
use crate::{FrameResult, OcrBoxResult, YStats, compute_box_y_stats};
use serde::Deserialize;
use serde::Serialize;

/// box 调整的置信度阈值参数（对齐 LocalDub `BoxAdjustedArgsSchema`）。
///
/// `box_adjusted_threshold`：confidence 低于此值的框进行 box 调整；省略时取默认 0.5。
/// 用 `Option<f32>` 保留「可省略」语义（对齐 zod 的 `.optional()` + `.default(0.5)`），
/// 通过 [`BoxAdjustedArgs::threshold`] 取值即自动补默认。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct BoxAdjustedArgs {
    /// box 调整的置信度阈值。默认 0.5。
    #[serde(
        default = "default_box_adjusted_threshold",
        rename = "boxAdjustedThreshold"
    )]
    pub box_adjusted_threshold: Option<f32>,
}

fn default_box_adjusted_threshold() -> Option<f32> {
    Some(0.5)
}

impl BoxAdjustedArgs {
    /// 解析实际生效的阈值：省略时为默认 0.5。
    pub fn threshold(&self) -> f32 {
        self.box_adjusted_threshold.unwrap_or(0.5)
    }
}

/// 调整后（行对齐）的单个字幕框：原 [`OcrBoxResult`] 全部字段 + 调整附加字段。
///
/// 用 `#[serde(flatten)]` 使序列化为平铺 JSON（对齐 TS 的 `OcrBoxResult & {...}` 交叉类型）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrBoxResultWithAdjust {
    #[serde(flatten)]
    pub base: OcrBoxResult,
    /// 上边界相对典型上边界的偏离（以行高为单位）。
    pub top_offset_ratio: f32,
    /// 下边界相对典型下边界的偏离（以行高为单位）。
    pub bot_offset_ratio: f32,
    /// 框高（像素）。
    pub height: f32,
    /// 框高相对典型行高的比值。
    pub height_ratio: f32,
    /// 是否离群（调整后置信度低于阈值）。
    pub is_outlier: bool,
    /// 经噪声惩罚调整后的置信度。
    pub adjusted_text_confidence: f32,
}

/// 调整后的一帧：原 [`FrameResult`]（去掉 `boxes`） + 调整后的 `boxes`。
///
/// 显式列出帧字段（而非 flatten `FrameResult`）以避免与下面的 `boxes` 字段在序列化时
/// 产生重复的 `boxes` key（对齐 TS 的 `Omit<FrameResult, "boxes"> & { boxes }`）。
#[derive(Clone, Debug, Serialize)]
pub struct FrameResultBoxWithAdjust {
    pub text: String,
    pub text_confidence: f64,
    pub x_range: [f32; 2],
    pub y_range: [f32; 2],
    pub timestamp: u64,
    pub boxes: Vec<OcrBoxResultWithAdjust>,
}

/// `build_ocr_frames_box_adjust` 的返回结构（对齐 LocalDub `OcrBoxAdjustResult`）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrBoxAdjustResult {
    /// 各帧调整后的结果。
    pub frames: Vec<FrameResultBoxWithAdjust>,
    /// 溯源 / 生成参数。
    pub meta: OcrBoxAdjustResultMeta,
}

/// 超集 → 子集的投影：`FrameResultBoxWithAdjust`（含调整附加字段）坍缩为干净的
/// [`FrameResult`]（仅保留识别结果，丢弃 adjust 字段）。
///
/// 这正是 `From` 的标准语义：宽类型到窄类型、明确丢弃附加字段的转换。
/// 实现后调用方可用 `.into()` 自动获得 [`FrameResult`]，对应 TS `get_ocr_frames_box_filtered`
/// 用 `as` 强转后 adjust 元数据实际丢失的语义。
impl From<FrameResultBoxWithAdjust> for FrameResult {
    fn from(f: FrameResultBoxWithAdjust) -> FrameResult {
        FrameResult {
            text: f.text,
            text_confidence: f.text_confidence,
            x_range: f.x_range,
            y_range: f.y_range,
            timestamp: f.timestamp,
            boxes: f.boxes.into_iter().map(|b| b.base).collect(),
        }
    }
}

/// `OcrBoxAdjustResult` 的 meta（对齐 LocalDub `OcrBoxAdjustResultMeta`）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrBoxAdjustResultMeta {
    /// 本次调整所用的纵向统计。
    pub y_stats: YStats,
    /// 帧数。
    pub frame_count: usize,
    /// 调整参数（原样回写，便于溯源）。
    pub args: BoxAdjustedArgs,
}

/// `get_ocr_frames_box_filtered` 的返回结构（对齐 LocalDub `OcrFramesBoxFilteredResult`）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrFramesBoxFilteredResult {
    /// 离群剔除后的干净帧。
    pub frames: Vec<FrameResult>,
    /// 溯源 / 生成参数。
    pub meta: OcrFramesBoxFilteredResultMeta,
}

/// `OcrFramesBoxFilteredResult` 的 meta（对齐 LocalDub `OcrFramesBoxFilteredResultMeta`）。
///
/// 注意：这里的 `y_stats` 是对**过滤后**的帧重新统计得到的（对齐 TS 用
/// `computeBoxYStats(filteredFrames)`），而非调整阶段传入的 `y_stats`。
#[derive(Clone, Debug, Serialize)]
pub struct OcrFramesBoxFilteredResultMeta {
    /// 对过滤后帧重新统计的纵向分布。
    pub y_stats: YStats,
    /// 帧数。
    pub frame_count: usize,
}

/// 对一组帧做字幕框调整：依据 [`YStats`] 估算的典型位置/行高，给每个框算
/// 上/下边界偏离比、框高比，按「偏离 >1 行高才罚」给噪声惩罚，得到调整后置信度；
/// 低于 `box_adjusted_threshold` 的框标记为离群。
///
/// 返回 [`OcrBoxAdjustResult`]（含 `meta`：`y_stats` / `frame_count` / `args`），
/// 对齐 LocalDub `build_ocr_frames_box_adjust`。坐标保持 f32，不取整。
pub fn build_ocr_frames_box_adjust(
    ocr_frames: &[FrameResult],
    y_stats: &YStats,
    args: &BoxAdjustedArgs,
) -> OcrBoxAdjustResult {
    let threshold = args.threshold();
    let frames: Vec<FrameResultBoxWithAdjust> = ocr_frames
        .iter()
        .map(|f| FrameResultBoxWithAdjust {
            text: f.text.clone(),
            text_confidence: f.text_confidence,
            x_range: f.x_range,
            y_range: f.y_range,
            timestamp: f.timestamp,
            boxes: f
                .boxes
                .iter()
                .map(|box_r| adjust_box(box_r, y_stats, threshold))
                .collect(),
        })
        .collect();
    OcrBoxAdjustResult {
        meta: OcrBoxAdjustResultMeta {
            y_stats: *y_stats,
            frame_count: frames.len(),
            args: *args,
        },
        frames,
    }
}

/// 单个框的预处理调整（对齐 TS `build_ocr_frames_box_adjust` 内的 map 体）。
fn adjust_box(box_r: &OcrBoxResult, y_stats: &YStats, threshold: f32) -> OcrBoxResultWithAdjust {
    // 空文本框：直接透传，不罚、不标记离群。
    if box_r.text.trim().is_empty() {
        return OcrBoxResultWithAdjust {
            base: box_r.clone(),
            top_offset_ratio: 0.0,
            bot_offset_ratio: 0.0,
            height: 0.0,
            height_ratio: 0.0,
            is_outlier: false,
            adjusted_text_confidence: box_r.text_confidence,
        };
    }

    let top = box_r.y_range[0];
    let bottom = box_r.y_range[1];
    let height = bottom - top;
    // 上下边界相对典型位置的偏离（以行高为单位）；行高无效时为 0。
    let top_or = if y_stats.median_height > 0.0 {
        (top - y_stats.mode[0]).abs() / y_stats.median_height
    } else {
        0.0
    };
    let bot_or = if y_stats.median_height > 0.0 {
        (bottom - y_stats.mode[1]).abs() / y_stats.median_height
    } else {
        0.0
    };
    let height_ratio = if y_stats.median_height > 0.0 {
        height / y_stats.median_height
    } else {
        0.0
    };
    let band_drift = top_or.max(bot_or); // 上下边界偏离取大的
    // 噪声惩罚：band 偏离 >1 行高才罚；高度比偏离也贡献。
    let noise_penalty = (band_drift - 1.0).max(0.0) * 0.5 + (1.0 - height_ratio).abs() * 0.3;
    let noise_penalty = noise_penalty.min(1.0);
    let adjusted = box_r.text_confidence * (1.0 - noise_penalty);
    let is_outlier = adjusted < threshold;

    OcrBoxResultWithAdjust {
        base: box_r.clone(),
        top_offset_ratio: top_or,
        bot_offset_ratio: bot_or,
        height,
        height_ratio,
        is_outlier,
        adjusted_text_confidence: adjusted,
    }
}

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
pub fn get_ocr_frames_box_filtered(
    frames: &[FrameResultBoxWithAdjust],
) -> OcrFramesBoxFilteredResult {
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

    fn box_with(text: &str, y_range: [f32; 2], conf: f32) -> OcrBoxResult {
        OcrBoxResult {
            text: text.into(),
            text_confidence: conf,
            box_confidence: conf,
            box_: [
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

    fn frame(boxes: Vec<OcrBoxResult>) -> FrameResult {
        FrameResult {
            text: String::new(),
            text_confidence: 0.0,
            boxes,
            x_range: [0.0, 0.0],
            y_range: [0.0, 0.0],
            timestamp: 0,
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
            top_offset_ratio: 0.0,
            bot_offset_ratio: 0.0,
            height: y_range[1] - y_range[0],
            height_ratio: 1.0,
            is_outlier,
            adjusted_text_confidence: conf,
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
    fn default_threshold_is_half() {
        assert_eq!(BoxAdjustedArgs::default().threshold(), 0.5);
    }

    #[test]
    fn explicit_threshold_overrides_default() {
        let a = BoxAdjustedArgs {
            box_adjusted_threshold: Some(0.3),
        };
        assert_eq!(a.threshold(), 0.3);
    }

    #[test]
    fn deserialize_omitted_field_uses_default() {
        let a: BoxAdjustedArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(a.threshold(), 0.5);
    }

    #[test]
    fn empty_text_box_passthrough() {
        let f = frame(vec![box_with("", [10.0, 20.0], 0.9)]);
        let y = YStats::default();
        let out = build_ocr_frames_box_adjust(&[f], &y, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(!b.is_outlier);
        assert_eq!(b.adjusted_text_confidence, 0.9);
        assert_eq!(b.height, 0.0);
        assert_eq!(out.meta.frame_count, 1);
    }

    #[test]
    fn far_from_mode_box_is_outlier() {
        // 典型位置 mode=[100,120]，行高 20；一个偏离很远的框应被罚成离群。
        let y = YStats {
            avg: [100.0, 120.0],
            mode: [100.0, 120.0],
            median: [100.0, 120.0],
            avg_height: 20.0,
            median_height: 20.0,
            mode_height: 20.0,
        };
        let f = frame(vec![box_with("a", [400.0, 420.0], 0.9)]);
        let out = build_ocr_frames_box_adjust(&[f], &y, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(b.is_outlier, "偏离典型位置过远的框应标记为离群");
        assert!(b.adjusted_text_confidence < 0.9);
        assert_eq!(b.height, 20.0);
        // meta 溯源字段正确回填。
        assert_eq!(out.meta.frame_count, 1);
        assert_eq!(out.meta.y_stats.median_height, 20.0);
        assert_eq!(out.meta.args.threshold(), 0.5);
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
        let out = get_ocr_frames_box_filtered(&[f]);
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
        let out = get_ocr_frames_box_filtered(&[f]);
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
        let out = get_ocr_frames_box_filtered(&[f]);
        assert_eq!(out.frames.len(), 1, "部分离群帧保留");
        assert_eq!(out.frames[0].timestamp, 999, "timestamp 保留原值");
        assert_eq!(out.frames[0].boxes.len(), 2, "离群框被剔除");
        assert_eq!(out.meta.frame_count, 1, "meta.frame_count 取过滤后帧数");
        // 输出为干净 FrameResult，框就是普通 OcrBoxResult（无 adjust 字段）。
        // meta.y_stats 应基于过滤后的帧重算：剩两个框 y=[100,120]/[200,220]。
        assert_eq!(out.meta.y_stats.median_height, 20.0);
    }
}
