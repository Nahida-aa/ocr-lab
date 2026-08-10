//! 命令行：`ocr-segment-adjust <input.json> [--out PATH]`
//!
//! 读入一个打包 JSON（结构见 [`Input`]），包含：
//! - `segments`：`OcrSegment[]`（合并后的字幕段，由 `merge-frames` 产出）；
//! - `frames`：`FrameResult[]`（逐帧识别结果，由主 CLI `/ adjust-box` 产出）；
//! - `video_height`：视频像素高度（Y 偏移惩罚归一化的分母）；
//! - `y_stats`：（可选）[`YStats`]，缺省时由 `frames` 经 `compute_box_y_stats` 推导；
//! - `args`：（可选）[`OcrSegmentAdjustArgs`]，缺省时全取默认。
//!
//! 跑 [`subtitle_ocr::ocr_segment_adjust`] 给每段补上 `adjusted_text_confidence` /
//! `y_penalty` / `iso_penalty`，输出 `OcrSegmentWithAdjust[]` 到 stdout（`--out` 额外落盘）。
//!
//! 对齐 LocalDub `computeSegmentAdjust(segments, frameResults, yStats, videoHeight, args)`：
//! 孤立惩罚依赖逐帧时间轴查找相邻非空帧，故 `frames` 必填；`y_stats` 缺省时按 TS 习惯
//! 由 `computeBoxYStats(frameResults)` 得到。

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;
use subtitle_ocr::{
    FrameResult, OcrBoxResult, OcrSegmentAdjustArgs, OcrSegmentWithAdjust, YStats,
    compute_box_y_stats, ocr_segment_adjust, SubtitlingSegment,
};
use tracing::info;

/// 输入里单个框（仅消费 [`OcrBoxResult`] 实际读取的字段；`OcrBoxResult` 未 derive
/// `Deserialize`，故单独定义 DTO，避免给上游 crate 强加 trait）。
#[derive(Debug, Deserialize)]
struct InputBox {
    text: String,
    #[serde(default)]
    text_confidence: f32,
    #[serde(default)]
    box_confidence: f32,
    /// 四个顶点（顺时针：左上、右上、右下、左下）；字段名为 `box`。
    #[serde(default, rename = "box")]
    box_: [[f32; 2]; 4],
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
            box_: self.box_,
            x_range: self.x_range,
            y_range: self.y_range,
            center: self.center,
        }
    }
}

/// 输入里单帧（仅消费 [`FrameResult`] 实际读取的字段）。
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

/// 输入里单条字幕段（仅消费 [`subtitle_ocr::OcrSegment`] 实际读取的字段；该类型未 derive
/// `Deserialize`，故单独定义 DTO）。
#[derive(Debug, Deserialize)]
struct InputSegment {
    text: String,
    start_ms: u64,
    end_ms: u64,
    #[serde(default)]
    y_range: Option<[f32; 2]>,
    #[serde(default)]
    text_confidence: f32,
    #[serde(default)]
    frame_count: Option<u32>,
    #[serde(default)]
    frames: Option<Vec<serde_json::Value>>,
}

impl InputSegment {
    fn into_ocr_segment(self) -> subtitle_ocr::OcrSegment {
        subtitle_ocr::OcrSegment {
            base: SubtitlingSegment {
                text: self.text,
                start_ms: self.start_ms,
                end_ms: self.end_ms,
            },
            y_range: self.y_range,
            text_confidence: self.text_confidence,
            frame_count: self.frame_count,
            frames: self.frames.map(|_| vec![]), // 帧明细不参与本步调整，置空占位。
        }
    }
}

/// 打包输入：段 + 逐帧结果 + 视频高度 + 可选 `y_stats` / `args`。
#[derive(Debug, Deserialize)]
struct Input {
    segments: Vec<InputSegment>,
    frames: Vec<InputFrame>,
    video_height: f32,
    #[serde(default)]
    y_stats: Option<YStats>,
    #[serde(default)]
    args: Option<OcrSegmentAdjustArgs>,
}

#[derive(Parser, Debug)]
#[command(
    name = "ocr-segment-adjust",
    about = "字幕段置信度调整：给合并段补 Y 偏移/孤立惩罚与调整后置信度"
)]
struct Cli {
    /// 打包输入 JSON 路径：含 `segments` / `frames` / `video_height`，可选 `y_stats` / `args`。
    input: PathBuf,

    /// 把调整结果额外写出到指定文件路径（同时仍向 stdout 打印）。便于落盘对接下游
    /// `ocr-segment-filter`。
    #[arg(long)]
    out: Option<PathBuf>,
}

/// 仓库根：二进制在 `target/debug/ocr-segment-adjust`，上溯两级到 workspace 根。
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
    let parsed: Input = serde_json::from_str(&raw)
        .context("解析打包 JSON 失败（需含 segments/frames/video_height）")?;

    let segments: Vec<subtitle_ocr::OcrSegment> = parsed
        .segments
        .into_iter()
        .map(InputSegment::into_ocr_segment)
        .collect();
    let frames: Vec<FrameResult> = parsed
        .frames
        .into_iter()
        .map(InputFrame::into_frame_result)
        .collect();

    // y_stats 缺省时由逐帧结果推导（对齐 TS `computeBoxYStats(frameResults)`）。
    let y_stats: YStats = match parsed.y_stats {
        Some(y) => y,
        None => compute_box_y_stats(&frames),
    };

    let args = parsed.args.unwrap_or_default();

    let result: Vec<OcrSegmentWithAdjust> =
        ocr_segment_adjust(&segments, &frames, &y_stats, parsed.video_height, &args);

    if let Some(out) = &cli.out {
        let path = resolve_path(&repo_root, out);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建输出目录失败: {}", parent.display()))?;
            }
        }
        let json =
            serde_json::to_string_pretty(&result).context("序列化 OcrSegmentWithAdjust[] 失败")?;
        std::fs::write(&path, json).with_context(|| format!("写入失败: {}", path.display()))?;
        info!(path = %path.display(), segments = result.len(), "已写出段");
    }

    // 主输出：调整后段 JSON 数组到 stdout。
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
