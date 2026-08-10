//! 命令行：`ocr-frames-filter-box <adjusted.json> [--out PATH]`
//!
//! 读入 `ocr-frames-adjust-box` 产出的调整后 JSON（`OcrBoxAdjustResult`，即
//! `FrameResultBoxWithAdjust[]` 带 `meta`），跑 [`subtitle_ocr::ocr_frames_filter_box`]
//! 剔除 `is_outlier` 的框、重聚合得到干净帧，输出 [`subtitle_ocr::OcrFramesBoxFilteredResult`]
//! （`{ frames, meta }`）到 stdout。
//!
//! 与 cpp 对齐：输入是 adjust 步骤的输出（含 `is_outlier` 标记），输出是离群剔除后的干净
//! 逐帧结果，可继续喂给 `merge-frames` 做时间轴合并。
//!
//! 耗时不在本 CLI 范围——这是离线后处理，无推理耗时可言；也不污染 stdout 的 JSON。

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;
use subtitle_ocr::{
    FrameResultBoxWithAdjust, OcrBoxResult, OcrBoxResultWithAdjust, OcrFramesBoxFilteredResult,
};
use tracing::info;

/// 输入里单个调整后框（镜像 [`OcrBoxResultWithAdjust`]：`base` 字段平铺 + 调整附加字段）。
/// `OcrBoxResultWithAdjust` 自身未 derive `Deserialize`，故单独定义 DTO。
#[derive(Debug, Deserialize)]
struct InputBoxWithAdjust {
    // —— base: OcrBoxResult 字段（经 serde flatten，平铺）——
    text: String,
    #[serde(default)]
    text_confidence: f32,
    #[serde(default)]
    box_confidence: f32,
    #[serde(default, rename = "box")]
    box_: [[f32; 2]; 4],
    #[serde(default)]
    x_range: [f32; 2],
    #[serde(default)]
    y_range: [f32; 2],
    #[serde(default)]
    center: [f32; 2],
    // —— 调整附加字段 ——
    #[serde(default)]
    top_offset_ratio: f32,
    #[serde(default)]
    bot_offset_ratio: f32,
    #[serde(default)]
    height: f32,
    #[serde(default)]
    height_ratio: f32,
    #[serde(default)]
    is_outlier: bool,
    #[serde(default)]
    adjusted_text_confidence: f32,
}

impl InputBoxWithAdjust {
    fn into_ocr_box_result_with_adjust(self) -> OcrBoxResultWithAdjust {
        OcrBoxResultWithAdjust {
            base: OcrBoxResult {
                text: self.text,
                text_confidence: self.text_confidence,
                box_confidence: self.box_confidence,
                box_: self.box_,
                x_range: self.x_range,
                y_range: self.y_range,
                center: self.center,
            },
            top_offset_ratio: self.top_offset_ratio,
            bot_offset_ratio: self.bot_offset_ratio,
            height: self.height,
            height_ratio: self.height_ratio,
            is_outlier: self.is_outlier,
            adjusted_text_confidence: self.adjusted_text_confidence,
        }
    }
}

/// 输入里单帧（镜像 [`FrameResultBoxWithAdjust`]）。
#[derive(Debug, Deserialize)]
struct InputFrameBoxWithAdjust {
    text: String,
    #[serde(default)]
    text_confidence: f64,
    #[serde(default)]
    x_range: [f32; 2],
    #[serde(default)]
    y_range: [f32; 2],
    #[serde(default)]
    timestamp: u64,
    #[serde(default)]
    boxes: Vec<InputBoxWithAdjust>,
}

impl InputFrameBoxWithAdjust {
    fn into_frame_result_box_with_adjust(self) -> FrameResultBoxWithAdjust {
        FrameResultBoxWithAdjust {
            text: self.text,
            text_confidence: self.text_confidence,
            x_range: self.x_range,
            y_range: self.y_range,
            timestamp: self.timestamp,
            boxes: self
                .boxes
                .into_iter()
                .map(InputBoxWithAdjust::into_ocr_box_result_with_adjust)
                .collect(),
        }
    }
}

/// 兼容两种输入形态：裸 `FrameResultBoxWithAdjust[]` 数组，或
/// `OcrBoxAdjustResult { frames, meta }`（adjust 步骤的输出形状）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputFrames {
    Wrapped { frames: Vec<InputFrameBoxWithAdjust> },
    Bare(Vec<InputFrameBoxWithAdjust>),
}

impl InputFrames {
    fn into_frames(self) -> Vec<FrameResultBoxWithAdjust> {
        match self {
            InputFrames::Wrapped { frames } => frames
                .into_iter()
                .map(InputFrameBoxWithAdjust::into_frame_result_box_with_adjust)
                .collect(),
            InputFrames::Bare(frames) => frames
                .into_iter()
                .map(InputFrameBoxWithAdjust::into_frame_result_box_with_adjust)
                .collect(),
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "ocr-frames-filter-box",
    about = "离群框过滤：剔除 is_outlier 框、重聚合得到干净帧"
)]
struct Cli {
    /// 调整后逐帧 JSON 文件路径（裸 `FrameResultBoxWithAdjust[]`，或 adjust 步骤输出的
    /// `OcrBoxAdjustResult { frames, meta }`）。由 `ocr-frames-adjust-box` 产出，对齐 LocalDub
    /// 的 adjust 输出。
    input: PathBuf,

    /// 把过滤结果额外写出到指定文件路径（同时仍向 stdout 打印）。便于落盘对接下游。
    #[arg(long)]
    out: Option<PathBuf>,
}

/// 仓库根：二进制在 `target/debug/ocr-frames-filter-box`，上溯两级到 workspace 根。
fn current_exe_repo_root() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("获取当前可执行文件路径失败")?;
    let exe_dir = exe.parent().context("可执行文件无父目录")?.to_path_buf();
    let root = exe_dir
        .join("..") // target/debug 或 target/release
        .join("..") // packages/subtitle-ocr
        .canonicalize()
        .context("解析仓库根失败（确认从仓库内构建）")?;
    Ok(root)
}

fn resolve_path(repo_root: &std::path::Path, p: &std::path::Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_root.join(p)
    }
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let repo_root = current_exe_repo_root()?;
    let input = resolve_path(&repo_root, &cli.input);

    let raw = std::fs::read_to_string(&input)
        .with_context(|| format!("读取输入文件失败: {}", input.display()))?;
    let parsed: InputFrames = serde_json::from_str(&raw)
        .context("解析调整后 JSON 失败（需为 FrameResultBoxWithAdjust[] 或 {frames,meta}）")?;
    let frames = parsed.into_frames();

    let result: OcrFramesBoxFilteredResult = subtitle_ocr::ocr_frames_filter_box(&frames);

    if let Some(out) = &cli.out {
        let path = resolve_path(&repo_root, out);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建输出目录失败: {}", parent.display()))?;
            }
        }
        let json =
            serde_json::to_string_pretty(&result).context("序列化 OcrFramesBoxFilteredResult 失败")?;
        std::fs::write(&path, json).with_context(|| format!("写入失败: {}", path.display()))?;
        info!(path = %path.display(), frames = result.meta.frame_count, "已写出帧");
    }

    // 主输出：过滤结果 JSON 到 stdout。
    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

/// 初始化 tracing subscriber：日志打到 stderr，级别由 `RUST_LOG` 控制（默认 `warn`）。
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
}
