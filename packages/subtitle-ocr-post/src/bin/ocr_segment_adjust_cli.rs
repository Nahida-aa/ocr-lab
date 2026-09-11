//! 命令行：`ocr-segment-adjust --segments <merged.json> --frames <filtered.json> [--video-height PX] [--out PATH]`
//!
//! 读两份输入 + 画面高度：
//! - `--segments`：`OcrSegment[]`（合并后的字幕段，`merge-frames` 输出）；
//! - `--frames`：`FrameResult[]`（干净逐帧识别结果，`ocr-frames-filter-box` 输出，
//!   已剔除离群框，供孤立惩罚 / Y 偏移统计用）；
//! - `--video-height`：画面像素高度（Y 偏移惩罚的分母）。省略时取 `--frames` 里的
//!   `meta.video_height`（识别侧写入；`ocr-frames-filter-box` 的输出不带该字段，故
//!   走这条链路时通常要显式给）。两者都缺则报错——本 CLI 不读视频文件。
//!
//! 跑 [`subtitle_ocr_post::ocr_segment_adjust`] 给每段补上 `adjusted_confidence` /
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
use subtitle_ocr_post::{
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
    fn into_segments(self) -> Vec<subtitle_ocr_post::OcrSegment> {
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

/// 输入 meta 里本 CLI 消费的字段（其余忽略）。
///
/// 注意：`ocr-frames-filter-box` 的输出 meta 只有 y_stats / frame_count、没有
/// `video_height`；只有直接喂原始 `frames.json`（识别侧产出）时才读得到。
#[derive(Debug, Deserialize)]
struct InputMeta {
    #[serde(default)]
    video_height: Option<u32>,
}

/// 兼容 frames 输入的两种形态：裸 `FrameResult[]` 数组，或 `{ frames, meta }`
///（`ocr-frames-filter-box` 输出形状）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputFrames {
    Wrapped {
        frames: Vec<InputFrame>,
        #[serde(default)]
        meta: Option<InputMeta>,
    },
    Bare(Vec<InputFrame>),
}

impl InputFrames {
    fn into_frames(self) -> Vec<FrameResult> {
        match self {
            InputFrames::Wrapped { frames, .. } => {
                frames.into_iter().map(InputFrame::into_frame_result).collect()
            }
            InputFrames::Bare(frames) => {
                frames.into_iter().map(InputFrame::into_frame_result).collect()
            }
        }
    }

    /// meta 里带的画面高度（filter-box 输出的 meta 没有此字段 → None）。
    fn meta_video_height(&self) -> Option<u32> {
        match self {
            InputFrames::Wrapped { meta, .. } => meta.as_ref().and_then(|m| m.video_height),
            InputFrames::Bare(_) => None,
        }
    }
}

/// 输入里单条字幕段（仅消费 [`subtitle_ocr_post::OcrSegment`] 实际读取的字段；该类型未 derive
/// `Deserialize`，故单独定义 DTO）。
#[derive(Debug, Deserialize)]
struct InputSegment {
    text: String,
    start_ms: u32,
    end_ms: u32,
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
    fn into_ocr_segment(self) -> subtitle_ocr_post::OcrSegment {
        subtitle_ocr_post::OcrSegment {
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

/// 取画面高度（Y 偏移惩罚归一化分母）：显式参数 > frames meta > 报错。
///
/// 不读视频文件：识别侧已把图像坐标系高度写进 `frames.json` 的 `meta.video_height`，
/// 为拿一个高度去链接 ffmpeg 不划算（且 `y_range` 本就是图像坐标系，视频高度在
/// 抽帧有缩放时反而是错的）。
fn resolve_video_height(explicit: Option<u32>, parsed: &InputFrames) -> Result<f32> {
    if let Some(h) = explicit {
        return Ok(h as f32);
    }
    if let Some(h) = parsed.meta_video_height() {
        return Ok(h as f32);
    }
    anyhow::bail!(
        "无法确定画面高度：frames JSON 的 meta.video_height 缺失（filter-box 的输出不带该字段），\
         请用 --video-height <px> 指定（ffprobe -v error -select_streams v:0 \
         -show_entries stream=height -of csv=p=0 <video>）"
    )
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

    /// 画面像素高度（Y 偏移惩罚归一化分母）。省略时取 frames JSON 的
    /// `meta.video_height`；两者都缺则报错（不读视频文件）。
    #[arg(long)]
    video_height: Option<u32>,

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

    let segments: Vec<subtitle_ocr_post::OcrSegment> = {
        let raw = std::fs::read_to_string(&segments_path)
            .with_context(|| format!("读取 segments 文件失败: {}", segments_path.display()))?;
        // 兼容裸数组或 { text, segments }（merge-frames 输出形状）。
        let parsed: InputSegments = serde_json::from_str(&raw)
            .context("解析 segments JSON 失败（需为 OcrSegment[] 或 {text,segments}）")?;
        parsed.into_segments()
    };
    // 高度与帧一起解析：meta 在 into_frames 时被丢弃，必须先取。
    let (frames, vh): (Vec<FrameResult>, f32) = {
        let raw = std::fs::read_to_string(&frames_path)
            .with_context(|| format!("读取 frames 文件失败: {}", frames_path.display()))?;
        // 兼容裸数组或 { frames, meta }（ocr-frames-filter-box 输出形状）。
        let parsed: InputFrames = serde_json::from_str(&raw)
            .context("解析 frames JSON 失败（需为 FrameResult[] 或 {frames,meta}）")?;
        let vh = resolve_video_height(cli.video_height, &parsed)?;
        (parsed.into_frames(), vh)
    };

    // y_stats 由逐帧结果推导（对齐 TS `computeBoxYStats(frameResults)`）。
    let y_stats: YStats = compute_box_y_stats(&frames);

    let result: Vec<OcrSegmentWithAdjust> =
        ocr_segment_adjust(&segments, &frames, &y_stats, vh, &OcrSegmentAdjustArgs::default());

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
