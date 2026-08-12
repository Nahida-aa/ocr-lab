//! 字幕框调整（行对齐后的离群剔除 / 置信度调整）的参数与入口。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `BoxAdjustedArgsSchema` /
//! `build_ocr_frames_box_adjust`（本库入口为 [`ocr_frames_adjust_box`]）：`box_adjusted_threshold`
//! 为触发 box 调整的置信度阈值
//! （低于此值的框进入调整流程），默认 0.5。

use crate::{FrameResult, OcrBoxResult, YStats};
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
    /// 经几何噪声惩罚调整后的置信度（`text×0.3 + box×0.7` 加权值 × (1 - penalty)）。
    pub adjusted_confidence: f32,
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

/// [`ocr_frames_adjust_box`] 的返回结构（对齐 LocalDub `OcrBoxAdjustResult`）。
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
/// 对齐 LocalDub `ocr_frames_adjust_box`。坐标保持 f32，不取整。
pub fn ocr_frames_adjust_box(
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
            adjusted_confidence: box_r.box_confidence,
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
    // 高度比项用「对数差」|log2(height_ratio)|（基准为 1 的比值，按倍率对称，
    // 缩小 3 倍与放大 3 倍等价偏离；且离 1 越远越陡，对高度异常小/大更敏感）。
    const HEIGHT_LOG_WEIGHT: f32 = 0.3;
    let height_pen = if height_ratio > 0.0 {
        height_ratio.log2().abs() * HEIGHT_LOG_WEIGHT
    } else {
        1.0 // 高度为 0 或非法 → 最大惩罚（必然离群）
    };
    // 噪声惩罚：band 偏离超过 BAND_THRESHOLD 行高即开始罚（原为 1 行高，对
    // 0.3/0.3 这种"上下各偏一点"的框毫无惩罚；降到 0.05 让任何明显偏移都计入）。
    // 权重 0.8（原 0.5）：中等偏移 + 低 text_confidence 的框（如 text 0.56/box 0.63、
    // 偏 0.28 行高）也能被压到阈值下。
    const BAND_THRESHOLD: f32 = 0.05;
    const BAND_WEIGHT: f32 = 0.8;
    let noise_penalty = ((band_drift - BAND_THRESHOLD).max(0.0) * BAND_WEIGHT + height_pen).min(1.0);
    // 几何异常反映检测框可疑，惩罚作用在「加权置信度」上：
    //   weighted = text_confidence × 0.3 + box_confidence × 0.7
    // 兼顾识别置信度与检测置信度（box 占主导，因几何惩罚主要针对检测框）。
    const TEXT_W: f32 = 0.3;
    const BOX_W: f32 = 0.7;
    let weighted_conf = box_r.text_confidence * TEXT_W + box_r.box_confidence * BOX_W;
    let adjusted = weighted_conf * (1.0 - noise_penalty);
    let is_outlier = adjusted < threshold;

    OcrBoxResultWithAdjust {
        base: box_r.clone(),
        top_offset_ratio: top_or,
        bot_offset_ratio: bot_or,
        height,
        height_ratio,
        is_outlier,
        adjusted_confidence: adjusted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OcrBoxResult;

    fn box_with(text: &str, y_range: [f32; 2], conf: f32) -> OcrBoxResult {
        box_with_conf(text, y_range, conf, conf)
    }

    fn box_with_conf(
        text: &str,
        y_range: [f32; 2],
        text_conf: f32,
        box_conf: f32,
    ) -> OcrBoxResult {
        OcrBoxResult {
            text: text.into(),
            text_confidence: text_conf,
            box_confidence: box_conf,
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
        let out = ocr_frames_adjust_box(&[f], &y, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(!b.is_outlier);
        assert_eq!(b.adjusted_confidence, 0.9);
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
        let out = ocr_frames_adjust_box(&[f], &y, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(b.is_outlier, "偏离典型位置过远的框应标记为离群");
        assert!(b.adjusted_confidence < 0.9);
        assert_eq!(b.height, 20.0);
        // meta 溯源字段正确回填。
        assert_eq!(out.meta.frame_count, 1);
        assert_eq!(out.meta.y_stats.median_height, 20.0);
        assert_eq!(out.meta.args.threshold(), 0.5);
    }

    #[test]
    fn abnormally_low_height_box_is_outlier() {
        // 典型位置 mode=[100,120]，行高 20。框高度异常小（height_ratio=0.1）时，
        // log2 高度惩罚应把它压成离群；band 偏离 <1 行高，不参与。
        let y = YStats {
            avg: [100.0, 120.0],
            mode: [100.0, 120.0],
            median: [100.0, 120.0],
            avg_height: 20.0,
            median_height: 20.0,
            mode_height: 20.0,
        };
        let f = frame(vec![box_with("a", [100.0, 102.0], 0.9)]);
        let out = ocr_frames_adjust_box(&[f], &y, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(b.is_outlier, "高度异常小的框应因 log2 高度惩罚被标记为离群");
        assert!(b.adjusted_confidence < 0.5);
        assert!(b.height_ratio < 0.5);
    }

    #[test]
    fn normal_height_box_not_penalized_extra() {
        // 高度正常的框（height_ratio=1.0，位置贴合典型）不应被 log2 高度惩罚误伤。
        let y = YStats {
            avg: [100.0, 120.0],
            mode: [100.0, 120.0],
            median: [100.0, 120.0],
            avg_height: 20.0,
            median_height: 20.0,
            mode_height: 20.0,
        };
        let f = frame(vec![box_with("a", [100.0, 120.0], 0.9)]);
        let out = ocr_frames_adjust_box(&[f], &y, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(!b.is_outlier, "高度正常且位置贴合的框不应被标记为离群");
        // band_drift=0、height_ratio=1 → 惩罚为 0，置信度不变。
        assert!((b.adjusted_confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn moderate_offset_low_conf_box_becomes_outlier() {
        // 复现实际遇到的「色」框：上下各偏 ~0.28/0.26 行高（没超 1 行高）、
        // text_confidence 低（0.56）。原 band 阈值 1.0 时这类框完全不被罚；
        // 阈值降到 0.05 + 加权置信度（text×0.3+box×0.7）后应被判离群。
        let y = YStats {
            avg: [100.0, 143.0],
            mode: [100.0, 143.0],
            median: [100.0, 143.0],
            avg_height: 43.0,
            median_height: 43.0,
            mode_height: 43.0,
        };
        // top = 100 - 0.279×43 ≈ 88, bottom = 143 - 0.256×43 ≈ 132。
        let f = frame(vec![box_with_conf("色", [88.0, 132.0], 0.564, 0.634)]);
        let out = ocr_frames_adjust_box(&[f], &y, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(b.is_outlier, "中等偏移 + 低 text_confidence 的框应被判离群");
        assert!(b.adjusted_confidence < 0.5);
    }
}
