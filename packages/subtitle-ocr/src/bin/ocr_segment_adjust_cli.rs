//! 命令行：`ocr-segment-adjust --segments <merged.json> --frames <filtered.json> --video <video> [--out PATH]`
//!
//! 读两份输入 + 视频路径：
//! - `--segments`：`OcrSegment[]`（合并后的字幕段，`merge-frames` 输出）；
//! - `--frames`：`FrameResult[]`（干净逐帧识别结果，`ocr-frames-filter-box` 输出，
//!   已剔除离群框，供孤立惩罚 / Y 偏移统计用）；
//! - `--video`：视频文件路径，用 ffmpeg 读视频像素高度（Y 偏移惩罚归一化的分母）。
//!
//! 跑 [`subtitle_ocr::ocr_segment_adjust`] 给每段补上 `adjusted_confidence` /
//! `y_penalty` / `iso_penalty`，输出 `OcrSegmentWithAdjust[]`。结果默认到 stdout；
//! 指定 `--out` 时落盘到文件、不再向 stdout 打印。
//!
//! 对齐 LocalDub `computeSegmentAdjust(segments, frameResults, yStats, videoHeight, args)`：
//! 孤立惩罚依赖逐帧时间轴查找相邻非空帧，故 `frames` 必填；`y_stats` 由
//! `computeBoxYStats(frameResults)` 推导。

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;
use subtitle_ocr::{
    FrameResult, OcrBoxResult, OcrSegmentAdjustArgs, OcrSegmentWithAdjust, YStats,
    compute_box_y_stats, ocr_segment_adjust, SubtitleSegment,
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
    /// 四个顶点（顺时针：左上、右上、右下、左下）；字段名为 `bbox`。
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

/// 兼容 segments 输入的两种形态：裸 `OcrSegment[]` 数组，或 `{ text, segments }`
///（`merge-frames` 输出形状）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputSegments {
    Wrapped { segments: Vec<InputSegment> },
    Bare(Vec<InputSegment>),
}

impl InputSegments {
    fn into_segments(self) -> Vec<subtitle_ocr::OcrSegment> {
        match self {
            InputSegments::Wrapped { segments } => {
                segments.into_iter().map(InputSegment::into_ocr_segment).collect()
            }
            InputSegments::Bare(segments) => {
                segments.into_iter().map(InputSegment::into_ocr_segment).collect()
            }
        }
    }
}

/// 兼容 frames 输入的两种形态：裸 `FrameResult[]` 数组，或 `{ frames, meta }`
///（`ocr-frames-filter-box` 输出形状）。
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
            base: SubtitleSegment {
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

/// 读视频文件获取像素高度（Y 偏移惩罚归一化分母），不手填。
fn video_height(video: &std::path::Path) -> Result<f32> {
    ffmpeg_next::init().context("ffmpeg 初始化失败")?;
    let ictx = ffmpeg_next::format::input(video).context("打开视频失败")?;
    let input = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or_else(|| anyhow::anyhow!("视频没有视频流"))?;
    let decoder = ffmpeg_next::codec::context::Context::from_parameters(input.parameters())
        .context("创建解码上下文失败")?
        .decoder()
        .video()
        .context("创建视频解码器失败")?;
    Ok(decoder.height() as f32)
}

#[derive(Parser, Debug)]
#[command(
    name = "ocr-segment-adjust",
    about = "字幕段置信度调整：给合并段补 Y 偏移/孤立惩罚与调整后置信度"
)]
struct Cli {
    /// 合并段 JSON 文件路径（`merge-frames` 输出，含 `segments`）。
    #[arg(long)]
    segments: PathBuf,

    /// 干净逐帧 JSON 文件路径（`ocr-frames-filter-box` 输出，含逐帧 `frames` 与 boxes）。
    #[arg(long)]
    frames: PathBuf,

    /// 视频文件路径：用于读取视频高度（Y 偏移惩罚归一化分母），不手填。
    #[arg(long)]
    video: PathBuf,

    /// 把调整结果写出到指定文件路径；指定后不再向 stdout 打印。便于落盘对接下游
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
    let segments_path = resolve_path(&repo_root, &cli.segments);
    let frames_path = resolve_path(&repo_root, &cli.frames);
    let video_path = resolve_path(&repo_root, &cli.video);

    let segments: Vec<subtitle_ocr::OcrSegment> = {
        let raw = std::fs::read_to_string(&segments_path)
            .with_context(|| format!("读取 segments 文件失败: {}", segments_path.display()))?;
        // 兼容裸数组或 { text, segments }（merge-frames 输出形状）。
        let parsed: InputSegments = serde_json::from_str(&raw)
            .context("解析 segments JSON 失败（需为 OcrSegment[] 或 {text,segments}）")?;
        parsed.into_segments()
    };
    let frames: Vec<FrameResult> = {
        let raw = std::fs::read_to_string(&frames_path)
            .with_context(|| format!("读取 frames 文件失败: {}", frames_path.display()))?;
        // 兼容裸数组或 { frames, meta }（ocr-frames-filter-box 输出形状）。
        let parsed: InputFrames = serde_json::from_str(&raw)
            .context("解析 frames JSON 失败（需为 FrameResult[] 或 {frames,meta}）")?;
        parsed.into_frames()
    };

    // y_stats 由逐帧结果推导（对齐 TS `computeBoxYStats(frameResults)`）。
    let y_stats: YStats = compute_box_y_stats(&frames);

    let video_height = video_height(&video_path)?;

    let result: Vec<OcrSegmentWithAdjust> =
        ocr_segment_adjust(&segments, &frames, &y_stats, video_height, &OcrSegmentAdjustArgs::default());

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
        // 显式打印落盘位置（绝对路径），方便确认输出去了哪（结果本身不打印到 stdout）。
        println!("已写入: {}", path.display());
    }

    // 主输出：调整后段 JSON 数组到 stdout。指定了 --out 时结果已落盘，不再向
    // stdout 重复打印（避免刷屏 + 与文件重复）。
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
