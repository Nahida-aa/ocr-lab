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
//!
//! 同模块还导出应用调整后的字幕段类型 [`OcrSegmentWithAdjust`]（在 [`OcrSegment`]
//! 基础上补充 `adjusted_text_confidence` / `y_penalty` / `iso_penalty` 三个可选字段）。

use crate::{FrameResult, OcrSegment, YStats};
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

/// 应用置信度调整后的字幕段（对齐 LocalDub `OcrSegmentWithAdjusted`）。
///
/// TS 用 `extends OcrSegment` 继承全部字段；Rust 无类型继承，用 `#[serde(flatten)]`
/// 内嵌 [`OcrSegment`]，序列化后与 TS 字段平铺一致。三个 `?` 字段（TS 可选）对应
/// `Option`，并以 `skip_serializing_if` 在输出时省略 `None`，与「可选字段缺省」语义对齐：
/// - `adjusted_text_confidence`：经 Y 偏移惩罚与孤立惩罚合成后的最终置信度；
/// - `y_penalty`：Y 偏移惩罚分量（0~1，越大偏移越严重）；
/// - `iso_penalty`：孤立程度惩罚分量（0~1，越大越孤立）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrSegmentWithAdjust {
    #[serde(flatten)]
    pub base: OcrSegment,
    /// 调整后的最终文本置信度（经 Y 偏移惩罚 + 孤立惩罚合成）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjusted_text_confidence: Option<f32>,
    /// Y 偏移惩罚分量（0~1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_penalty: Option<f32>,
    /// 孤立程度惩罚分量（0~1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso_penalty: Option<f32>,
}

/// 把逐段 [`OcrSegment`] 调整出最终置信度（对齐 LocalDub `utils.ts` 的 `computeSegmentAdjust`）。
///
/// 惩罚由两部分加权合成（各自独立计算，见 [`compute_y_penalty`] / [`compute_iso_penalty`]）：
/// - Y 偏移惩罚 `y_penalty`：段质心距平均质心 `avgCentroid` 的偏移，归一化到
///   `offset / (videoHeight × adjustYFactor)`，clamp 到 [0,1]。
/// - 孤立惩罚 `iso_penalty`：仅对单帧段（`frame_count == 1`）计算——取段中点 `mid` 在
///   非空帧时间轴里相邻的「前/后最近非空帧」的较小间隔，归一化到 `gap / isoThresholdMs`；
///   若某侧无相邻非空帧，间隔视为无穷 → 惩罚取满 1。
///
/// 总惩罚 `= adjustYWeight·y_penalty + adjustIsoWeight·iso_penalty`，调整后置信度
/// `= text_confidence × max(0, 1 - totalPenalty)`，三个值均四舍五入到 2 位小数。
///
/// 早退：段为空，或 `yStats.avg` 全为 0（无有效纵向统计）时，直接把各段原样包成
/// [`OcrSegmentWithAdjust`] 返回（不计算惩罚，对齐 TS 早退语义）。
///
/// 类型映射：TS 的 `frame_count` / `text_confidence` 为可选（本库 `frame_count` 为
/// `Option<u32>`、`text_confidence` 为必填 f32）；当 `frame_count` 为 `None` 时按 TS 的
/// 「缺字段」处理——原样包回、不计算调整。
pub fn ocr_segment_adjust(
    segments: &[OcrSegment],
    frame_results: &[FrameResult],
    y_stats: &YStats,
    video_height: f32,
    args: &OcrSegmentAdjustArgs,
) -> Vec<OcrSegmentWithAdjust> {
    // 早退守卫：对齐 TS `segments.length === 0 || (!yStats.avg[0] && yStats.avg[1] === 0)`。
    if segments.is_empty() || (y_stats.avg[0] == 0.0 && y_stats.avg[1] == 0.0) {
        return segments
            .iter()
            .map(|s| OcrSegmentWithAdjust {
                base: s.clone(),
                adjusted_text_confidence: None,
                y_penalty: None,
                iso_penalty: None,
            })
            .collect();
    }

    let avg_centroid = (y_stats.avg[0] + y_stats.avg[1]) / 2.0;

    // 非空帧（有 text 与 x/y 值域）的时间戳升序，供孤立度查找。
    let mut non_empty_ts: Vec<u64> = frame_results
        .iter()
        .filter(|f| !f.text.is_empty() && f.x_range != [0.0, 0.0] && f.y_range != [0.0, 0.0])
        .map(|f| f.timestamp)
        .collect();
    non_empty_ts.sort_unstable();

    segments
        .iter()
        .map(|seg| {
            // 对齐 TS `seg.frame_count === undefined || seg.text_confidence === undefined`：
            // 本库 text_confidence 必填，仅 frame_count 可能为 None。
            if seg.frame_count.is_none() {
                return OcrSegmentWithAdjust {
                    base: seg.clone(),
                    adjusted_text_confidence: None,
                    y_penalty: None,
                    iso_penalty: None,
                };
            }

            let y_penalty =
                compute_y_penalty(seg, avg_centroid, video_height, args.adjust_y_factor());
            let iso_penalty = compute_iso_penalty(seg, &non_empty_ts, args.iso_threshold_ms());

            // ─── 合成最终置信度 ───
            let total_penalty = args.adjust_y_weight() as f64 * y_penalty as f64
                + args.adjust_iso_weight() as f64 * iso_penalty as f64;
            let adjusted_confidence = seg.text_confidence as f64 * (1.0 - total_penalty).max(0.0);

            OcrSegmentWithAdjust {
                base: seg.clone(),
                adjusted_text_confidence: Some(round2(adjusted_confidence)),
                y_penalty: Some(round2(y_penalty as f64)),
                iso_penalty: Some(round2(iso_penalty as f64)),
            }
        })
        .collect()
}

/// 计算单段的 Y 偏移惩罚（对齐 TS `computeSegmentAdjust` 内联的 y 惩罚分支）。
///
/// 段质心 `(y_range[0]+y_range[1])/2` 距 `avg_centroid` 的偏移，归一化到
/// `offset / (videoHeight × adjust_y_factor)`，clamp 到 [0,1]；段无 `y_range` 时返回 0。
/// `videoHeight × adjust_y_factor` 为归一化分母，分母 ≤ 0 时退化返回 0（避免除零）。
fn compute_y_penalty(
    seg: &OcrSegment,
    avg_centroid: f32,
    video_height: f32,
    adjust_y_factor: f32,
) -> f32 {
    let y_range = match seg.y_range {
        Some(y) => y,
        None => return 0.0,
    };
    let centroid = (y_range[0] + y_range[1]) / 2.0;
    let offset = (centroid - avg_centroid).abs();
    let denom = video_height * adjust_y_factor;
    if denom > 0.0 {
        (offset / denom).min(1.0)
    } else {
        0.0
    }
}

/// 计算单段的孤立惩罚（对齐 TS `computeSegmentAdjust` 内联的 iso 惩罚分支）。
///
/// 仅对单帧段（`frame_count == 1`）有意义：取段中点 `mid` 在升序非空帧时间轴
/// `non_empty_ts` 里相邻的「前/后最近非空帧」的较小间隔，归一化到
/// `gap / iso_threshold_ms`，clamp 到 [0,1]；某侧无相邻非空帧时该间隔视为无穷 → 惩罚取满 1。
/// 多帧段（`frame_count != 1`）返回 0。
fn compute_iso_penalty(seg: &OcrSegment, non_empty_ts: &[u64], iso_threshold_ms: u64) -> f32 {
    if seg.frame_count != Some(1) {
        return 0.0;
    }
    let mid = (seg.base.start_ms + seg.base.end_ms) / 2;
    // 前最近非空帧：反向找第一个 < mid。
    let non_empty_before = non_empty_ts.iter().rev().find(|&&t| t < mid).copied();
    // 后最近非空帧：正向找第一个 > mid。
    let non_empty_after = non_empty_ts.iter().find(|&&t| t > mid).copied();
    // 无相邻则间隔为无穷 → 惩罚取满 1（对齐 TS Infinity/threshold = 1）。
    let nearest_gap: f64 = match (non_empty_before, non_empty_after) {
        (Some(b), Some(a)) => (mid - b).min(a - mid) as f64,
        (Some(b), None) => (mid - b) as f64,
        (None, Some(a)) => (a - mid) as f64,
        (None, None) => f64::INFINITY,
    };
    ((nearest_gap / iso_threshold_ms as f64).min(1.0)) as f32
}

/// 四舍五入到 2 位小数（对齐 TS `Math.round(x * 100) / 100`）。
fn round2(x: f64) -> f32 {
    ((x * 100.0).round() / 100.0) as f32
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

    #[test]
    fn ocr_segment_with_adjusted_serializes_flattened_with_optional_omitted() {
        // 验证 OcrSegmentWithAdjusted 序列化后字段平铺（extends OcrSegment 语义），
        // 且三个调整字段为 None 时不出现在 JSON 中。
        let seg = OcrSegmentWithAdjust {
            base: crate::OcrSegment {
                base: crate::SubtitlingSegment {
                    text: "你好".into(),
                    start_ms: 100,
                    end_ms: 200,
                },
                y_range: Some([10.0, 30.0]),
                text_confidence: 0.9,
                frame_count: Some(2),
                frames: None,
            },
            adjusted_text_confidence: None,
            y_penalty: None,
            iso_penalty: None,
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["text"], "你好");
        assert_eq!(v["start_ms"], 100);
        assert_eq!(v["end_ms"], 200);
        assert_eq!(v["text_confidence"], 0.9);
        assert!(
            v.get("adjusted_text_confidence").is_none(),
            "可选字段 None 应省略"
        );
        assert!(v.get("y_penalty").is_none());
        assert!(v.get("iso_penalty").is_none());
    }

    #[test]
    fn ocr_segment_with_adjusted_serializes_with_optional_present() {
        let seg = OcrSegmentWithAdjust {
            base: crate::OcrSegment {
                base: crate::SubtitlingSegment {
                    text: "你好".into(),
                    start_ms: 100,
                    end_ms: 200,
                },
                y_range: None,
                text_confidence: 0.9,
                frame_count: None,
                frames: None,
            },
            adjusted_text_confidence: Some(0.7),
            y_penalty: Some(0.1),
            iso_penalty: Some(0.2),
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["adjusted_text_confidence"], 0.7);
        assert_eq!(v["y_penalty"], 0.1);
        assert_eq!(v["iso_penalty"], 0.2);
    }

    /// 构造一个带文本/时刻/y 值域/置信度/帧数的段（其余字段占位）。
    fn seg(
        text: &str,
        start: u64,
        end: u64,
        y: [f32; 2],
        conf: f32,
        fc: Option<u32>,
    ) -> OcrSegment {
        OcrSegment {
            base: crate::SubtitlingSegment {
                text: text.into(),
                start_ms: start,
                end_ms: end,
            },
            y_range: Some(y),
            text_confidence: conf,
            frame_count: fc,
            frames: None,
        }
    }

    /// 构造一个非空帧（带 text + x/y 值域）。
    fn frame(ts: u64, y: [f32; 2]) -> FrameResult {
        FrameResult {
            text: "x".into(),
            text_confidence: 0.9,
            boxes: vec![],
            x_range: [0.0, 10.0],
            y_range: y,
            timestamp: ts,
        }
    }

    fn y_stats(avg_top: f32, avg_bottom: f32) -> YStats {
        YStats {
            avg: [avg_top, avg_bottom],
            mode: [avg_top, avg_bottom],
            median: [avg_top, avg_bottom],
            avg_height: avg_bottom - avg_top,
            median_height: avg_bottom - avg_top,
            mode_height: avg_bottom - avg_top,
        }
    }

    #[test]
    fn compute_segment_adjust_early_return_when_empty_or_zero_ystats() {
        let args = OcrSegmentAdjustArgs::default();
        // 段为空 → 原样包回（无惩罚字段）。
        let out = ocr_segment_adjust(&[], &[], &y_stats(10.0, 30.0), 1080.0, &args);
        assert!(out.is_empty());
        // yStats.avg 全 0 → 原样包回，不计算惩罚。
        let segs = vec![seg("你好", 0, 100, [10.0, 30.0], 0.9, Some(1))];
        let out = ocr_segment_adjust(&segs, &[], &y_stats(0.0, 0.0), 1080.0, &args);
        assert_eq!(out.len(), 1);
        assert!(out[0].adjusted_text_confidence.is_none());
        assert!(out[0].y_penalty.is_none());
    }

    #[test]
    fn compute_segment_adjust_single_frame_with_neighbor_has_low_iso_penalty() {
        // 单帧段 mid=500，相邻非空帧 400 与 600 → gap=100，isoThreshold=1500 → penalty≈0.07。
        let frames = vec![frame(400, [10.0, 30.0]), frame(600, [10.0, 30.0])];
        let segs = vec![seg("你好", 400, 600, [10.0, 30.0], 0.9, Some(1))];
        let args = OcrSegmentAdjustArgs::default();
        let out = ocr_segment_adjust(&segs, &frames, &y_stats(10.0, 30.0), 1080.0, &args);
        let iso = out[0].iso_penalty.unwrap();
        // 100/1500=0.0666… 经 round2 取 0.07。
        assert!((iso - 0.07).abs() < 1e-6, "iso_penalty ≈ 0.07, got {iso}");
        // 段质心=(10+30)/2=20 与 avgCentroid=20 重合 → y_penalty=0；
        // 调整置信度 = 0.9 × (1 - (0.8·0 + 0.2·0.07)) = 0.9 × 0.986 = 0.8874 → round2 0.89。
        assert_eq!(out[0].y_penalty.unwrap(), 0.0);
        assert!((out[0].adjusted_text_confidence.unwrap() - 0.89).abs() < 1e-6);
    }

    #[test]
    fn compute_segment_adjust_fully_isolated_single_frame_gets_full_iso_penalty() {
        // 单帧段 mid=5000，无任何非空帧 → 间隔无穷 → iso_penalty=1。
        let frames: Vec<FrameResult> = vec![]; // 无相邻帧
        let segs = vec![seg("你好", 4000, 6000, [10.0, 30.0], 0.9, Some(1))];
        let args = OcrSegmentAdjustArgs::default();
        let out = ocr_segment_adjust(&segs, &frames, &y_stats(10.0, 30.0), 1080.0, &args);
        assert_eq!(out[0].iso_penalty.unwrap(), 1.0, "全孤立单帧惩罚取满 1");
        // 调整置信度 = 0.9 × max(0, 1 - (0.8·0 + 0.2·1)) = 0.9 × 0.8 = 0.72。
        assert!((out[0].adjusted_text_confidence.unwrap() - 0.72).abs() < 1e-6);
    }

    #[test]
    fn compute_segment_adjust_multi_frame_has_no_iso_penalty() {
        // 多帧段（frame_count>1）不计算孤立惩罚 → iso_penalty=0。
        let frames = vec![frame(400, [10.0, 30.0])];
        let segs = vec![seg("你好", 0, 1000, [10.0, 30.0], 0.9, Some(3))];
        let args = OcrSegmentAdjustArgs::default();
        let out = ocr_segment_adjust(&segs, &frames, &y_stats(10.0, 30.0), 1080.0, &args);
        assert_eq!(out[0].iso_penalty.unwrap(), 0.0, "多帧段无孤立惩罚");
    }

    #[test]
    fn compute_segment_adjust_y_penalty_from_offset() {
        // avgCentroid=20，段质心=60（y_range [50,70]）→ offset=40，videoHeight=1080,
        // adjustYFactor 默认 0.08 → denom=86.4 → yPenalty=40/86.4≈0.46。
        let segs = vec![seg("你好", 0, 100, [50.0, 70.0], 0.9, Some(2))];
        let args = OcrSegmentAdjustArgs::default();
        let out = ocr_segment_adjust(&segs, &[], &y_stats(10.0, 30.0), 1080.0, &args);
        let yp = out[0].y_penalty.unwrap();
        // 40/86.4=0.46296… 经 round2 取 0.46。
        assert!((yp - 0.46).abs() < 1e-2, "y_penalty ≈ 0.46, got {yp}");
    }

    #[test]
    fn compute_segment_adjust_frame_count_none_returns_unadjusted() {
        // frame_count 为 None → 对齐 TS 缺字段，原样包回。
        let segs = vec![seg("你好", 0, 100, [10.0, 30.0], 0.9, None)];
        let args = OcrSegmentAdjustArgs::default();
        let out = ocr_segment_adjust(&segs, &[], &y_stats(10.0, 30.0), 1080.0, &args);
        assert!(out[0].adjusted_text_confidence.is_none());
        assert!(out[0].y_penalty.is_none());
        assert!(out[0].iso_penalty.is_none());
    }

    #[test]
    fn compute_y_penalty_zero_when_no_y_range() {
        // 段无 y_range → 惩罚为 0。
        let seg_no_y = OcrSegment {
            base: crate::SubtitlingSegment {
                text: "x".into(),
                start_ms: 0,
                end_ms: 100,
            },
            y_range: None,
            text_confidence: 0.9,
            frame_count: Some(1),
            frames: None,
        };
        assert_eq!(compute_y_penalty(&seg_no_y, 20.0, 1080.0, 0.08), 0.0);
    }

    #[test]
    fn compute_y_penalty_offset_normalized() {
        // 段质心 60、avgCentroid 20 → offset=40，denom=1080×0.08=86.4 → 40/86.4≈0.46。
        let s = seg("你好", 0, 100, [50.0, 70.0], 0.9, Some(1));
        let yp = compute_y_penalty(&s, 20.0, 1080.0, 0.08);
        assert!(
            (yp - 40.0 / 86.4).abs() < 1e-3,
            "y_penalty ≈ 0.46, got {yp}"
        );
    }

    #[test]
    fn compute_y_penalty_clamped_to_one() {
        // 超大偏移 + 极小 denom → clamp 到 1。
        let s = seg("你好", 0, 100, [0.0, 1000.0], 0.9, Some(1));
        let yp = compute_y_penalty(&s, 0.0, 10.0, 0.08); // denom=0.8, offset=500 → 625 → 1
        assert_eq!(yp, 1.0);
    }

    #[test]
    fn compute_iso_penalty_only_for_single_frame() {
        // 多帧段 → 0，不论有无相邻帧。
        let multi = seg("你好", 0, 1000, [10.0, 30.0], 0.9, Some(3));
        assert_eq!(compute_iso_penalty(&multi, &[400, 600], 1500), 0.0);
    }

    #[test]
    fn compute_iso_penalty_gap_normalized() {
        // 单帧 mid=500，相邻 400/600 → gap=100，threshold=1500 → 100/1500≈0.0666…（f32 近似）。
        let single = seg("你好", 400, 600, [10.0, 30.0], 0.9, Some(1));
        let iso = compute_iso_penalty(&single, &[400, 600], 1500);
        assert!(
            (iso - 100.0 / 1500.0).abs() < 1e-6,
            "iso_penalty ≈ 0.0666, got {iso}"
        );
    }

    #[test]
    fn compute_iso_penalty_fully_isolated_is_one() {
        // 单帧 mid=5000，无相邻帧 → 间隔无穷 → 1。
        let single = seg("你好", 4000, 6000, [10.0, 30.0], 0.9, Some(1));
        assert_eq!(compute_iso_penalty(&single, &[], 1500), 1.0);
    }
}
