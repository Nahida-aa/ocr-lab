//! 命令行：`ocr-frames-adjust-box <frames.json> [--box-adjusted-threshold F] [--out PATH]`
//!
//! 读入主 CLI（`subtitle-ocr`）产出的逐帧 JSON（裸 `FrameResult[]` 数组，或
//! `OcrFramesResult { frames, meta }`），先按各帧框统计纵向分布得到 [`YStats`]，再跑
//! [`subtitle_ocr::ocr_frames_adjust_box`] 给每个框算偏离/噪声惩罚、标记离群，输出
//! [`subtitle_ocr::OcrBoxAdjustResult`]（`{ frames, meta }`）。结果默认到 stdout；
//! 指定 `--out` 时落盘到文件、不再向 stdout 打印（结果较大）。
//!
//! 与 cpp 对齐：输入是 `asr_ocr_frames.json` / `sf_ocr_frames.json` 这类含 `boxes` 的逐帧
//! JSON（调整依赖框几何，故必须带 `boxes`）；输出是调整后的框与 meta。
//!
//! 耗时不在本 CLI 范围——这是离线后处理，无推理耗时可言；也不污染 stdout 的 JSON。

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;
use subtitle_ocr::{
    BoxAdjustedArgs, FrameResult, OcrBoxAdjustResult, OcrBoxResult, XStats, YStats,
    compute_box_x_stats, compute_box_y_stats, ocr_frames_adjust_box,
};
use tracing::info;

/// 输入里单个框（仅反序列化 [`OcrBoxResult`] 实际消费的字段；`OcrBoxResult` 自身未 derive
/// `Deserialize`，故这里单独定义 DTO，避免给上游 crate 强加 trait）。其余字段以默认值补位。
#[derive(Debug, Deserialize)]
struct InputBox {
    text: String,
    #[serde(default)]
    text_confidence: f32,
    #[serde(default)]
    box_confidence: f32,
    /// 四个顶点（顺时针：左上、右上、右下、左下）；TS/上游 JSON 里字段名为 `bbox`。
    #[serde(default, rename = "bbox")]
    bbox: [[f32; 2]; 4],
    #[serde(default)]
    x_range: [f32; 2],
    #[serde(default)]
    y_range: [f32; 2],
    #[serde(default)]
    center: [f32; 2],
}

impl InputBox {
    fn into_ocr_box_result(self) -> OcrBoxResult {
        OcrBoxResult {
            text: self.text,
            text_confidence: self.text_confidence,
            box_confidence: self.box_confidence,
            bbox: self.bbox,
            x_range: self.x_range,
            y_range: self.y_range,
            center: self.center,
        }
    }
}

/// 输入里单帧（含 boxes，调整依赖框几何）。
#[derive(Debug, Deserialize)]
struct InputFrame {
    text: String,
    #[serde(default)]
    text_confidence: f64,
    #[serde(default)]
    boxes: Vec<InputBox>,
    #[serde(default)]
    x_range: [f32; 2],
    #[serde(default)]
    y_range: [f32; 2],
    #[serde(default)]
    timestamp: u64,
}

impl InputFrame {
    fn into_frame_result(self) -> FrameResult {
        FrameResult {
            text: self.text,
            text_confidence: self.text_confidence,
            boxes: self.boxes.into_iter().map(InputBox::into_ocr_box_result).collect(),
            x_range: self.x_range,
            y_range: self.y_range,
            timestamp: self.timestamp,
        }
    }
}

/// 兼容两种输入形态：裸 `FrameResult[]` 数组，或 `OcrFramesResult { frames, meta }`。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputFrames {
    Wrapped { frames: Vec<InputFrame> },
    Bare(Vec<InputFrame>),
}

impl InputFrames {
    fn into_frames(self) -> Vec<FrameResult> {
        match self {
            InputFrames::Wrapped { frames } => {
                frames.into_iter().map(InputFrame::into_frame_result).collect()
            }
            InputFrames::Bare(frames) => {
                frames.into_iter().map(InputFrame::into_frame_result).collect()
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "ocr-frames-adjust-box",
    about = "字幕框调整：按纵向统计给框算偏离/噪声惩罚、标记离群"
)]
struct Cli {
    /// 逐帧 JSON 文件路径（裸 `FrameResult[]` 数组，或 `OcrFramesResult { frames, meta }`）。
    /// 须含 `boxes`（调整依赖框几何），由主 CLI（`subtitle-ocr --out ...`）产出，对齐
    /// LocalDub 的 `asr_ocr_frames.json`。
    input: PathBuf,

    /// box 调整的置信度阈值：调整后置信度低于此值的框标记为离群。默认 0.5。
    #[arg(long)]
    box_adjusted_threshold: Option<f32>,

    /// 把调整结果写出到指定文件路径；指定后不再向 stdout 打印（结果较大）。便于落盘对接下游。
    #[arg(long)]
    out: Option<PathBuf>,
}

/// 仓库根：二进制在 `target/debug/ocr-frames-adjust-box`，上溯两级到 workspace 根。
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
    let parsed: InputFrames =
        serde_json::from_str(&raw).context("解析逐帧 JSON 失败（需为 FrameResult[] 或 {frames,meta}）")?;
    let frames = parsed.into_frames();

    // 先按各帧框统计纵向分布（对齐 TS 的 y_stats 来源）与横向分布。
    let y_stats: YStats = compute_box_y_stats(&frames);
    let x_stats: XStats = compute_box_x_stats(&frames);

    let args = BoxAdjustedArgs {
        box_adjusted_threshold: cli.box_adjusted_threshold,
    };

    let result: OcrBoxAdjustResult =
        ocr_frames_adjust_box(&frames, &y_stats, &x_stats, &args);

    if let Some(out) = &cli.out {
        let path = resolve_path(&repo_root, out);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建输出目录失败: {}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(&result).context("序列化 OcrBoxAdjustResult 失败")?;
        std::fs::write(&path, json).with_context(|| format!("写入失败: {}", path.display()))?;
        info!(path = %path.display(), frames = result.frames.len(), "已写出帧");
        // 显式打印落盘位置（绝对路径），方便确认输出去了哪（结果本身不打印到 stdout）。
        println!("已写入: {}", path.display());
    }

    // 主输出：调整结果 JSON 到 stdout。指定了 --out 时结果已落盘，不再向
    // stdout 重复打印（结果较大，避免刷屏 + 与文件重复）。
    if cli.out.is_none() {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

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
