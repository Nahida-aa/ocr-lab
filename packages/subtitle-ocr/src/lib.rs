//! # subtitle-ocr
//!
//! 字幕 OCR 专用层，构建在 [`rapidocr_ort::OcrEngine`]（PP-OCR det/rec/cls）之上。
//!
//! 本包不做模型推理，只做「字幕场景」的专属逻辑，与 cpp 实现
//! （`packages/subtitle-ocr-cpp/ocr_pipeline.cpp`）对齐，便于 bench 公平对比：
//!
//! - **bottom_only ROI**：只送画面底部 40% 给引擎，提速（cpp 默认开启）。
//! - **subtitle_only y 过滤**：仅保留 y 中心落在底部比例区间的字幕框。
//! - **NMS 去重**：剔除被大框高度覆盖的重叠框（cpp `--no-nms` 可关）。
//! - **多帧合并 + 计时**：相邻帧同文本合并成带 `start/end` 的段（LocalDub
//!   `mergeFrames` 风格）。
//!
//! 计时口径：模型只加载一次，推理耗时由调用方自行测量（在 `ocr_image` 调用前后
//! `Instant::now()` 即可）。rapidocr-ort 的 `detect` 把 det/rec 合成一次调用，无法
//! 单独计时，故本包不内置计时。这些耗时是旁路观测数据，不进入 JSON 输出。

use anyhow::Result;
use ndarray::{Array3, s};
use rapidocr_ort::{ModelProfile, OcrEngine};
use serde::Serialize;
use std::path::PathBuf;

pub(crate) mod geometry;
pub(crate) mod ocr_fix;
pub(crate) mod ocr_util;
pub(crate) mod pipeline;

// 模块保持 pub(crate)（内部分层是实现细节），仅把对外 API 提到 crate 根，
// 使用方路径仍是 `subtitle_ocr::aggregate_boxes` / `subtitle_ocr::nms`，
// 不随内部拆分而变。
pub use crate::geometry::nms;
pub use crate::ocr_fix::box_adjust::{
    BoxAdjustedArgs, FrameResultBoxWithAdjust, OcrBoxAdjustResult, OcrBoxAdjustResultMeta,
    OcrBoxResultWithAdjust, OcrFramesBoxFilteredResult, OcrFramesBoxFilteredResultMeta,
    build_ocr_frames_box_adjust, filter_ocr_frames_box,
};
pub use crate::ocr_fix::merge_frames::{
    MergeFramesArgs, MergeFramesResult, OcrSegment, SegmentFrame, avg_confidence,
    base_merge_frames, dedup_overlap, edit_distance, is_substring_of, merge_adjacent_same_text,
    merge_confidence, merge_frames, merge_substring_segments, normalize, overlap,
    remove_triplet_noise,
};
pub use crate::ocr_fix::segment_adjust::{
    OcrSegmentAdjustArgs, OcrSegmentWithAdjust, compute_segment_adjust,
};
pub use crate::ocr_fix::segment_filter::{
    OcrSegmentFilterData, OcrSegmentFilterMeta, OcrSegmentFilterResult, ocr_segment_filter,
    ocr_segment_filter_with_meta,
};
pub use crate::ocr_fix::stats::{YStats, compute_box_y_stats};
pub use crate::ocr_fix::subtitling::SubtitlingSegment;
pub use crate::ocr_util::aggregate_boxes;
pub use crate::pipeline::{OcrDevice, OcrFramesMeta, OcrFramesResult};

// ==========================================================
// 选项与结果类型
// ==========================================================

/// 字幕 OCR 的行为开关（对齐 cpp 的 CLI 参数）。
#[derive(Clone, Debug)]
pub struct OcrOptions {
    /// 只裁底部 40% 送 OCR（cpp 默认 true）。
    pub bottom_only: bool,
    /// 仅保留 y 中心在画面底部比例区间的字幕框（cpp `--subtitle-only`）。
    pub subtitle_only: bool,
    /// 重叠框 NMS 去重（cpp 默认 true，`--no-nms` 关闭）。
    pub use_nms: bool,
    /// 识别置信度下限（cpp `text_score`，默认 0.5）。
    pub text_score: f32,
    /// 是否用 cpp 同款的透视矫正裁剪（warpPerspective）替代轴对齐包围盒。
    /// 配合 det 几何 minAreaRect 一起用（两者耦合）；默认 false。
    pub use_warp_crop: bool,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            bottom_only: true,
            subtitle_only: false,
            use_nms: true,
            text_score: 0.5,
            use_warp_crop: false,
        }
    }
}

/// 单个文字识别区域（`rapidocr_ort::OcrBoxResult` 的 re-export）。
///
/// 原先是自定义 `FrameLine` 结构体，字段几乎与 rapidocr-ort 的 `OcrResult`
/// 相同（text/confidence/box_），仅多了 `y_center`（= `center[1]`）。后改为
/// 类型别名，并统一命名为 `OcrBoxResult`（表示"一个识别区域/文本框"）。
/// ⚠️ 坐标语义：`ocr_image` 返回前会把 box/center 的 y 加回 `y_offset`，
/// 还原成原图坐标。
pub use rapidocr_ort::OcrBoxResult;

/// 单图聚合结果（可携带单时刻时间戳）。
///
/// 把一张图里识别出的多框聚合成一条文本 + 值域 + 明细。`timestamp` 为
/// 单时刻（毫秒）：默认 `0` 表示「无时间」；当上游按文件名（`ms` / `ms_ms`）
/// 或帧序号代入时携带该图对应时刻。`ms_ms` 文件名会让同一张图被识别一次、
/// 产出两个 `FrameResult`（各自带 start/end 时刻、内容相同）。
///
/// 本库仍只吃一张图、不知道图片整体来源结构；把多个 `FrameResult` 合并成
/// 带时间轴的字幕段由独立合并层 `subtitle-ocr-merge` 负责。
#[derive(Clone, Debug, Serialize)]
pub struct FrameResult {
    /// 该图识别文本（多行按出现顺序拼接，用空格分隔）。
    pub text: String,
    /// 该图聚合置信度：各框 `text_confidence` 的均值（对齐下游 TS `aggregate_boxes`，
    /// 多框取平均而非最大）。
    pub text_confidence: f64,
    /// 该图所有识别区域明细（每行文本/框/score，含坐标还原）。
    pub boxes: Vec<OcrBoxResult>,
    /// 横向值域 `[min_x, max_x]`（像素坐标），无字幕时为 `[0,0]`。
    pub x_range: [f32; 2],
    /// 纵向值域 `[min_y, max_y]`（像素坐标），无字幕时为 `[0,0]`。
    pub y_range: [f32; 2],
    /// 该图对应时刻（毫秒）。`0` 表示无时间（如单图 `<image>` 调用、或
    /// `aggregate_boxes` 纯聚合后尚未赋值）。
    ///
    /// 为何不交给 `aggregate_boxes` 填：同一张图可能对应多个时刻（`ms_ms` 文件名
    /// 的 start/end），为避免对同一 boxes 重复聚合，聚合函数保持单参数、只产出
    /// 无时间结果；调用方聚合一次后，按各时刻 `clone` 并覆写本字段。时间来源由
    /// 上游按文件名 `ms`/`ms_ms` 解析、帧序号或视频 PTS 提供。
    pub timestamp: u64,
}

// ===========================================================================
// 引擎封装
// ===========================================================================

/// 字幕 OCR 引擎：持有 [`OcrEngine`] 与行为选项。
pub struct SubtitleOcr {
    engine: OcrEngine,
    opts: OcrOptions,
}

impl SubtitleOcr {
    /// 按模型套件构建（模型目录默认仓库根 `models/rapidocr`）。
    pub fn from_profile(
        profile: ModelProfile,
        model_dir: &std::path::Path,
        opts: OcrOptions,
    ) -> Result<Self> {
        let engine =
            OcrEngine::from_profile(profile, model_dir)?.with_warp_crop(opts.use_warp_crop);
        Ok(Self { engine, opts })
    }

    /// 对一帧 BGR 图像（H×W×3，0-255 u8，读图见 [`rapidocr_ort::load_image`]）做字幕 OCR，返回排序后的识别行。
    ///
    /// 流程对齐 cpp `runOcr`：bottom_only ROI → subtitle_only y 过滤 → NMS。
    pub fn ocr_image(&mut self, rgb: &Array3<u8>) -> Result<Vec<OcrBoxResult>> {
        let (h, _, _) = rgb.dim();
        let h = h as i64;

        // ---- 1. bottom_only：裁底部 40% 作为 ROI ----
        let y_offset = if self.opts.bottom_only {
            ((h as f32) * 0.6) as i64
        } else {
            0
        };
        let roi: Array3<u8> = if y_offset > 0 {
            rgb.slice(s![y_offset as usize.., .., ..]).to_owned()
        } else {
            rgb.clone()
        };

        // ---- 2. 引擎推理（det + rec + cls）----
        let results: Vec<OcrBoxResult> = self.engine.detect(&roi)?;

        // ---- 3. 后处理：还原坐标 / y 过滤 / NMS / trim / 排序 ----
        let mut boxes: Vec<OcrBoxResult> = results
            .into_iter()
            .map(|mut r| {
                // ROI 坐标还原回原图：box 每点 y 与 center.y 都加 y_offset。
                if y_offset > 0 {
                    for p in &mut r.box_ {
                        p[1] += y_offset as f32;
                    }
                    r.center[1] += y_offset as f32;
                }
                r.text = r.text.trim().to_string();
                r
            })
            .filter(|r| {
                // subtitle_only：y 中心须落在画面底部 [0.85, 0.99]（cpp 比值口径）。
                if self.opts.subtitle_only {
                    let ratio = r.center[1] / (h as f32);
                    if !(0.85..=0.99).contains(&ratio) {
                        return false;
                    }
                }
                !r.text.is_empty() && r.text_confidence >= self.opts.text_score
            })
            .collect();

        if self.opts.use_nms && boxes.len() > 1 {
            boxes = geometry::nms(boxes);
        }

        // 排序：先按 y 中心，差 ≤20px 再按 x 中心（cpp 的 TL/BR 排序等价）。
        boxes.sort_by(|a, b| {
            let ya = a.center[1];
            let yb = b.center[1];
            if (ya - yb).abs() > 20.0 {
                ya.partial_cmp(&yb).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                let xa = a.box_[0][0];
                let xb = b.box_[0][0];
                xa.partial_cmp(&xb).unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        Ok(boxes)
    }
}

// ===========================================================================
// 批量入口：把「图片路径 + 时刻」列表跑完 OCR 并聚合
// ===========================================================================

/// 一张图对应的时刻，固化 `ms` / `ms_ms` 文件名约定（取代裸 `Vec<u64>`）。
///
/// - [`FrameTimes::None`]：无时间（单图 `<image>` 调用，或尚待赋值）；
///   展开为单个 `0` 时刻，产出一个 `FrameResult`。
/// - [`FrameTimes::Single`]`(t)`：单时刻（`ms` 文件名）。
/// - [`FrameTimes::Range`]`(s, e)`：时间区间（`ms_ms` 文件名）；同一张图仅识别一次，
///   展开为 `[s, e]` 两个 `FrameResult`（内容相同、仅 `timestamp` 不同）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameTimes {
    None,
    Single(u64),
    Range(u64, u64),
}

impl FrameTimes {
    /// 排序 / 去重用的主时刻（区间取起点）。
    pub fn sort_key(&self) -> u64 {
        match self {
            FrameTimes::None => 0,
            FrameTimes::Single(t) => *t,
            FrameTimes::Range(s, _) => *s,
        }
    }
}

/// 一张待识别图片及其对应时刻（批量入口 [`ocr_entries`] 的输入单元）。
pub struct OcrEntry {
    pub path: PathBuf,
    pub times: FrameTimes,
}

/// 对一组 [`OcrEntry`] 跑 OCR 并聚合，返回按时刻展开的 [`FrameResult`] 列表。
///
/// 这是「读图 → 识别 → 聚合 → 按时刻展开」的核心流程，供 CLI / benchmark / 测试
/// 直接复用，无需各自照抄。每张图**仅识别一次**：`ms_ms` 同一张图产出多个
/// `FrameResult`，内容相同、仅 `timestamp` 不同（避免对同一 boxes 重复聚合）。
///
/// 本函数不含：耗时测量（调用方在 `ocr_image` 前后自行 `Instant`）、JSON 序列化、
/// 目录扫描 / 文件名解析（这些留给调用方；CLI 见 `subtitle_ocr::util::list_frames`）。
pub fn ocr_entries(ocr: &mut SubtitleOcr, entries: &[OcrEntry]) -> Result<Vec<FrameResult>> {
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let rgb = rapidocr_ort::load_image(&e.path)?;
        let boxes = ocr.ocr_image(&rgb)?;
        let aggregated = aggregate_boxes(&boxes);
        // 按 times 的形态展开：直接 match 枚举，无时刻 / 单时刻都只产出一个
        // FrameResult 且无需 clone；只有 Range 才复制一份（内容相同、仅时刻不同）。
        match e.times {
            FrameTimes::None => out.push(aggregated),
            FrameTimes::Single(t) => out.push(FrameResult {
                timestamp: t,
                ..aggregated
            }),
            FrameTimes::Range(s, end) => {
                out.push(FrameResult {
                    timestamp: s,
                    ..aggregated.clone()
                });
                out.push(FrameResult {
                    timestamp: end,
                    ..aggregated
                });
            }
        }
    }
    Ok(out)
}
