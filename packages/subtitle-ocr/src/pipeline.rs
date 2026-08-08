//! OCR 流水线的产物类型：把多帧 [`FrameResult`] 打包成一次 stage 的原始输出。
//!
//! 对应 LocalDub `packages/subtitle-ocr/types.ts` 的 `OcrFramesResult`，供
//! `asr_ocr_frames.json` / `sf_ocr_frames.json` / `fixed_fps_ocr_frames.json`
//! 这类「asr_ocr 阶段原始帧输出」落地使用。

use serde::Serialize;

use crate::FrameResult;

/// OCR 运行设备。对齐 LocalDub `OcrDevice`。
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrDevice {
    Cpu,
    Cuda,
    Directml,
    Coreml,
    Rocm,
    Mps,
}

/// `ocr_frames.json` 的元数据（溯源 / 生成参数）。对齐 LocalDub `OcrFramesMeta`。
#[derive(Clone, Debug, Serialize)]
pub struct OcrFramesMeta {
    /// OCR 引擎名称，如 `ort-cpp` / `ort-rust`。
    pub engine: String,
    /// OCR 运行设备。
    pub device: OcrDevice,
}

/// 一次 stage 的原始 OCR 帧输出（`asr_ocr_frames.json | sf_ocr_frames.json | fixed_fps_ocr_frames.json` 等）。
///
/// 对齐 LocalDub `OcrFramesResult`：`frames` 为各帧聚合结果，`meta` 记录溯源信息。
#[derive(Clone, Debug, Serialize)]
pub struct OcrFramesResult {
    pub frames: Vec<FrameResult>,
    pub meta: OcrFramesMeta,
}
