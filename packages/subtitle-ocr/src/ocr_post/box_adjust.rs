//! 字幕框调整（行对齐后的离群剔除 / 置信度调整）的参数与入口。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `BoxAdjustedArgsSchema` /
//! `build_ocr_frames_box_adjust`（本库入口为 [`ocr_frames_adjust_box`]）：`box_adjusted_threshold`
//! 为触发 box 调整的置信度阈值
//! （低于此值的框进入调整流程），默认 0.5。

use crate::{FrameResult, OcrBoxResult, XStats, YStats};
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
    /// 框中心 y 相对典型中心（mode 中心）的偏离（以行高为单位）。
    /// 配合 [`height_ratio`] 即可完整描述框相对典型字幕带的位置与大小。
    pub y_center_offset_ratio: f32,
    /// 框中心 x 相对典型 x 中心（mode）的偏离（以行高为单位）。
    /// 与 [`y_center_offset_ratio`] 共同构成「出界」判定：横向偏移过大（如贴边噪声框）
    /// 也会被罚。
    pub x_center_offset_ratio: f32,
    /// 框高（像素）。
    pub height: f32,
    /// 框高相对典型行高的比值。
    pub height_ratio: f32,
    /// 噪声惩罚分解（每项均经饱和放缩 `raw/(raw+C)` 映射到 `[0,1)`，严格小于 1，
    /// 不硬截断，便于三者直接比较大小）：
    /// y 方向中心偏移贡献 = `saturate((|y_center_offset_ratio| - BAND_THRESHOLD).max(0) * BAND_WEIGHT)`。
    pub y_penalty: f32,
    /// x 方向中心偏移贡献的惩罚（同 [`y_penalty`] 算法，作用于 x 偏移）。
    pub x_penalty: f32,
    /// 高度比偏离贡献的惩罚（`saturate(|log2(height_ratio)| * HEIGHT_LOG_WEIGHT)`，高度为 0 则饱和到近 1）。
    pub height_penalty: f32,
    /// 实际生效的总惩罚（= `saturate(y_raw + x_raw + h_raw)`，同样严格 <1），
    /// 直接决定 `adjusted_confidence = weighted × (1 - total_penalty)`。
    pub total_penalty: f32,
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
    /// 本次调整所用的横向统计。
    pub x_stats: XStats,
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

/// 对一组帧做字幕框调整：依据 [`YStats`] 估算的典型纵向位置/行高、[`XStats`]
/// 估算的典型横向中心，给每个框算中心偏离比、框高比，按偏离给噪声惩罚，得到调整后
/// 置信度；低于 `box_adjusted_threshold` 的框标记为离群。
///
/// 返回 [`OcrBoxAdjustResult`]（含 `meta`：`y_stats` / `x_stats` / `frame_count` / `args`），
/// 对齐 LocalDub `ocr_frames_adjust_box`。坐标保持 f32，不取整。
pub fn ocr_frames_adjust_box(
    ocr_frames: &[FrameResult],
    y_stats: &YStats,
    x_stats: &XStats,
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
                .map(|box_r| adjust_box(box_r, y_stats, x_stats, threshold))
                .collect(),
        })
        .collect();
    OcrBoxAdjustResult {
        meta: OcrBoxAdjustResultMeta {
            y_stats: *y_stats,
            x_stats: *x_stats,
            frame_count: frames.len(),
            args: *args,
        },
        frames,
    }
}

/// 单个框的预处理调整（对齐 TS `build_ocr_frames_box_adjust` 内的 map 体）。
fn adjust_box(
    box_r: &OcrBoxResult,
    y_stats: &YStats,
    x_stats: &XStats,
    threshold: f32,
) -> OcrBoxResultWithAdjust {
    // 空文本框：直接透传，不罚、不标记离群。
    if box_r.text.trim().is_empty() {
        return OcrBoxResultWithAdjust {
            base: box_r.clone(),
            y_center_offset_ratio: 0.0,
            x_center_offset_ratio: 0.0,
            height: 0.0,
            height_ratio: 0.0,
            y_penalty: 0.0,
            x_penalty: 0.0,
            height_penalty: 0.0,
            total_penalty: 0.0,
            is_outlier: false,
            adjusted_confidence: box_r.box_confidence,
        };
    }

    let top = box_r.y_range[0];
    let bottom = box_r.y_range[1];
    let height = bottom - top;
    let height_ratio = if y_stats.median_height > 0.0 {
        height / y_stats.median_height
    } else {
        0.0
    };
    // 框中心 y 相对典型中心（mode 中心）的偏离（以行高为单位）。
    // 旧实现分别算上下边界偏离再取 max，会丢失「中心没偏但整框偏矮」的信号，
    // 且与 height_ratio 重复表达高度维度；这里改为单一中心偏移，配合 height_ratio
    // 即可正交地描述「位置 + 大小」，更简洁。
    let y_center_offset_ratio = if y_stats.median_height > 0.0 {
        let mode_center = (y_stats.mode[0] + y_stats.mode[1]) / 2.0;
        (box_r.center[1] - mode_center) / y_stats.median_height
    } else {
        0.0
    };
    // 框中心 x 相对典型 x 中心（mode）的偏离（以行高为单位，量纲与 y 一致，
    // 便于并入同一 band_drift）。用于捕捉横向贴边/跑出主流字幕水平区段的噪声框。
    let x_center_offset_ratio = if y_stats.median_height > 0.0 {
        (box_r.center[0] - x_stats.mode) / y_stats.median_height
    } else {
        0.0
    };

    // 噪声惩罚阈值/权重（对齐注释中的调参结论）。
    const BAND_THRESHOLD: f32 = 0.05;
    const BAND_WEIGHT: f32 = 0.8;
    const HEIGHT_LOG_WEIGHT: f32 = 0.3;
    // 饱和放缩常数 C：原始惩罚 raw 达到 C 时，放缩后惩罚为 0.5（半饱和点）。
    // raw = (|offset| - THRESH).max(0) * W（位置项）或 |log2(height_ratio)| * WH（高度项），
    // 二者量纲均为「惩罚强度」，故共用同一 C。
    const SAT_C: f32 = 1.0;
    /// 饱和放缩：把无界的原始惩罚 `raw ≥ 0` 平滑映射到 `[0, 1)`（永远严格小于 1，
    /// 不会溢出），`raw=0 → 0`、`raw=C → 0.5`、`raw→∞ → 1`（渐近）。
    /// 用 `raw/(raw+C)` 而非硬截断 `.min(1.0)`，避免惩罚在 1.0 处突变卡死。
    fn saturate(raw: f32) -> f32 {
        let r = raw.max(0.0);
        r / (r + SAT_C)
    }
    // 三项原始惩罚（线性、可能 >1），各自经饱和放缩到 [0,1)；y/x 各算各的，
    // 便于排查「哪个因子主导了离群判定」：
    //   y_penalty / x_penalty：位置偏移超过阈值的线性贡献。
    //   height_penalty：高度比偏离的对数贡献（0 高度 → raw 置 1.0 必离群）。
    let y_raw = ((y_center_offset_ratio.abs() - BAND_THRESHOLD).max(0.0)) * BAND_WEIGHT;
    let x_raw = ((x_center_offset_ratio.abs() - BAND_THRESHOLD).max(0.0)) * BAND_WEIGHT;
    let h_raw = if height_ratio > 0.0 {
        height_ratio.log2().abs() * HEIGHT_LOG_WEIGHT
    } else {
        1.0 // 高度为 0 或非法 → 最大原始惩罚（必然离群）
    };
    let y_penalty = saturate(y_raw);
    let x_penalty = saturate(x_raw);
    let height_penalty = saturate(h_raw);
    // 总惩罚：三项原始惩罚求和后做一次饱和放缩（同样严格 <1，不硬截断）。
    // 这等价于「任一维度异常都贡献惩罚、叠加后渐近封顶」，比逐项截断再求和更平滑。
    let total_raw = y_raw + x_raw + h_raw;
    let total_penalty = saturate(total_raw);
    // 几何异常反映检测框可疑，惩罚作用在「加权置信度」上：
    //   weighted = text_confidence × 0.3 + box_confidence × 0.7
    // 兼顾识别置信度与检测置信度（box 占主导，因几何惩罚主要针对检测框）。
    const TEXT_W: f32 = 0.3;
    const BOX_W: f32 = 0.7;
    let weighted_conf = box_r.text_confidence * TEXT_W + box_r.box_confidence * BOX_W;
    let adjusted = weighted_conf * (1.0 - total_penalty);
    let is_outlier = adjusted < threshold;

    OcrBoxResultWithAdjust {
        base: box_r.clone(),
        y_center_offset_ratio,
        x_center_offset_ratio,
        height,
        height_ratio,
        y_penalty,
        x_penalty,
        height_penalty,
        total_penalty,
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
        let out = ocr_frames_adjust_box(
            &[f],
            &y,
            &XStats {
                avg: 5.0,
                mode: 5.0,
                median: 5.0,
            },
            &BoxAdjustedArgs::default(),
        );
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
        let out = ocr_frames_adjust_box(
            &[f],
            &y,
            &XStats {
                avg: 5.0,
                mode: 5.0,
                median: 5.0,
            },
            &BoxAdjustedArgs::default(),
        );
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
        let out = ocr_frames_adjust_box(
            &[f],
            &y,
            &XStats {
                avg: 5.0,
                mode: 5.0,
                median: 5.0,
            },
            &BoxAdjustedArgs::default(),
        );
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
        let out = ocr_frames_adjust_box(
            &[f],
            &y,
            &XStats {
                avg: 5.0,
                mode: 5.0,
                median: 5.0,
            },
            &BoxAdjustedArgs::default(),
        );
        let b = &out.frames[0].boxes[0];
        assert!(!b.is_outlier, "高度正常且位置贴合的框不应被标记为离群");
        // band_drift=0、height_ratio=1 → 惩罚为 0，置信度不变。
        assert!((b.adjusted_confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn real_frame_lu_ding_perfect_not_outlier() {
        // 真实数据回归点（低优先级守护）：取自 tmp/大/20/sf_ocr_fix 的一帧字幕框
        // "时间到第一名陆鼎成绩完美"（bbox [416,606]-[865,640]，center [640.5,623]）。
        // 该框 y 比主流字幕带（mode [625,672]、行高 46）略高一点，但仍是合法字幕，
        // 不应被判离群。固化其真实惩罚分解，防止 box 调整算法回归把它误伤。
        //
        // ⚠️ 维护提示（低优先级）：本测试是「真实合法字幕框」的代理。若某次改动后
        // 它开始 `is_outlier == true` 或 `adjusted_confidence` 明显下挫，先别急着修数值
        // 让测试通过——这很可能是算法对「略偏但合法的字幕框」变苛刻了（如 x 维度
        // 误伤、阈值过严）。应先审视算法本身是否退化，再决定是否调整本回归基线。
        let y = YStats {
            avg: [625.0, 672.0],
            mode: [625.0, 672.0],
            median: [625.0, 672.0],
            avg_height: 46.0,
            median_height: 46.0,
            mode_height: 46.0,
        };
        let x = XStats {
            avg: 639.5,
            mode: 639.5,
            median: 639.5,
        };
        let box_r = crate::OcrBoxResult {
            text: "时间到第一名陆鼎成绩完美".into(),
            text_confidence: 0.99626046,
            box_confidence: 0.87568265,
            bbox: [
                [416.0, 606.0],
                [865.0, 606.0],
                [865.0, 640.0],
                [416.0, 640.0],
            ],
            x_range: [416.0, 865.0],
            y_range: [606.0, 640.0],
            center: [640.5, 623.0],
        };
        let f = FrameResult {
            text: String::new(),
            text_confidence: 0.0,
            boxes: vec![box_r],
            x_range: [0.0, 0.0],
            y_range: [0.0, 0.0],
            timestamp: 0,
        };
        let out = ocr_frames_adjust_box(&[f], &y, &x, &BoxAdjustedArgs::default());
        let b = &out.frames[0].boxes[0];
        assert!(!b.is_outlier, "真实字幕框「时间到第一名陆鼎成绩完美」不应被判离群");
        // 与 tmp/大/20 的真实 adjust 产物逐项对齐（容差 1e-3）。
        assert!((b.y_center_offset_ratio + 0.5543478).abs() < 1e-3);
        assert!((b.x_center_offset_ratio - 0.02173913).abs() < 1e-3);
        assert!((b.height_ratio - 0.73913044).abs() < 1e-3);
        assert!((b.y_penalty - 0.2874845).abs() < 1e-3);
        assert!((b.height_penalty - 0.115693584).abs() < 1e-3);
        assert!((b.total_penalty - 0.34824038).abs() < 1e-3);
        assert!((b.adjusted_confidence - 0.59431094).abs() < 1e-3);
    }
}
