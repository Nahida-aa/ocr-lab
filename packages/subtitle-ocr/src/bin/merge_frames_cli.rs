//! 命令行：`merge-frames <frames.json> [--is-merge-substring] [--dedup-edit-distance N]`
//!
//! 读入主 CLI（`subtitle-ocr`）产出的逐帧 JSON（裸 `FrameResult[]` 数组，或
//! `OcrFramesResult { frames, meta }`），跑 [`subtitle_ocr::merge_frames`] 的多段流水线，
//! 把逐帧结果合并成带时间轴的字幕段 [`subtitle_ocr::MergeFramesResult`]。结果默认到
//! stdout；指定 `--out` 时落盘到文件、不再向 stdout 打印。
//!
//! 与 cpp 对齐：输入是 `asr_ocr_frames.json` / `sf_ocr_frames.json` 这类逐帧 JSON，
//! 输出是合并后的 `{ text, segments }`。
//!
//! 耗时不在本 CLI 范围——这是离线后处理，无推理耗时可言；也不污染 stdout 的 JSON。

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;
use subtitle_ocr::{
    FrameResult, MergeFramesArgs, MergeFramesResult,
};
use tracing::info;

/// 仅反序列化 `merge_frames` 实际消费的字段（不依赖上游 `OcrBoxResult` 的 `Deserialize`）。
/// 其余字段以默认值补位，构造出完整 [`FrameResult`]。
#[derive(Debug, Deserialize)]
struct InputFrame {
    text: String,
    #[serde(default)]
    text_confidence: f64,
    #[serde(default)]
    y_range: Option<[f32; 2]>,
    #[serde(default)]
    timestamp: u64,
}

impl InputFrame {
    fn into_frame_result(self) -> FrameResult {
        FrameResult {
            text: self.text,
            text_confidence: self.text_confidence,
            boxes: vec![],
            x_range: [0.0, 0.0],
            // 主 CLI 在无字幕时写 [0,0]；这里 None 统一视为 [0,0]，与「无值域」语义一致。
            y_range: self.y_range.unwrap_or([0.0, 0.0]),
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
    name = "merge-frames",
    about = "多帧合并：把逐帧 OCR 结果合并成带时间轴的字幕段"
)]
struct Cli {
    /// 逐帧 JSON 文件路径（裸 `FrameResult[]` 数组，或 `OcrFramesResult { frames, meta }`）。
    /// 由主 CLI（`subtitle-ocr --out ...`）产出，对齐 LocalDub 的 `asr_ocr_frames.json`。
    input: PathBuf,

    /// 是否合并互为子串的相邻文本（Pass 1）。省略时默认 `false`。
    #[arg(long)]
    is_merge_substring: bool,

    /// dedupOverlap 的编辑距离阈值（Pass 3）：编辑距离 ≤ 此值则合并。默认 `1`。
    #[arg(long)]
    dedup_edit_distance: Option<u32>,

    /// 把合并结果写出到指定文件路径；指定后不再向 stdout 打印。便于落盘对接下游。
    #[arg(long)]
    out: Option<PathBuf>,
}

/// 仓库根：二进制在 `target/debug/merge-frames`，上溯两级到 workspace 根。
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

    let args = MergeFramesArgs {
        is_merge_substring: Some(cli.is_merge_substring),
        dedup_edit_distance: cli.dedup_edit_distance,
    };

    let result: MergeFramesResult = subtitle_ocr::merge_frames(&frames, &args);

    if let Some(out) = &cli.out {
        let path = resolve_path(&repo_root, out);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建输出目录失败: {}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(&result).context("序列化 MergeFramesResult 失败")?;
        std::fs::write(&path, json).with_context(|| format!("写入失败: {}", path.display()))?;
        info!(path = %path.display(), segments = result.segments.len(), "已写出段");
        // 显式打印落盘位置（绝对路径），方便确认输出去了哪（结果本身不打印到 stdout）。
        println!("已写入: {}", path.display());
    }

    // 主输出：合并结果 JSON 到 stdout。指定了 --out 时结果已落盘，不再向
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
