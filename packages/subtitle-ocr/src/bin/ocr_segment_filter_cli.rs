//! 命令行：`ocr-segment-filter <adjusted.json> [--text-confidence-threshold F] [--bare] [--out PATH]`
//!
//! 读入 `ocr-segment-adjust` 产出的调整后 JSON（`OcrSegmentWithAdjust[]` 数组，或
//! `OcrSegmentFilterResult { meta, result }`），跑 [`subtitle_ocr::ocr_segment_filter`] /
//! [`subtitle_ocr::ocr_segment_filter_with_meta`] 按置信度阈值过滤字幕段，把低于阈值的段丢弃，
//! 输出到 stdout。
//!
//! 置信度优先级（对齐 TS `ocrSegmentFilter`）：段若带 `adjusted_text_confidence` 则优先用它，
//! 否则退回 `text_confidence`；阈值 ≤ 0 时不过滤。`--bare` 输出纯 `OcrSegmentWithAdjust[]`
//! 数组（便于回灌下游），否则输出带 `meta` / `result` 的 `OcrSegmentFilterResult`。
//!
//! 与 cpp 对齐：输入是 adjust 步骤的输出（含 `adjusted_text_confidence`），输出是过滤后的
//! 字幕段，可继续喂给字幕拼装 / 导出。

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;
use subtitle_ocr::{OcrSegmentFilterResult, OcrSegmentWithAdjust, SubtitleSegment};
use tracing::info;

/// 输入里单条调整后字幕段（镜像 [`OcrSegmentWithAdjust`]：`base`（`OcrSegment`，其内再 flatten
/// `SubtitleSegment`）字段平铺 + 三个调整附加字段）。`OcrSegmentWithAdjust` 未 derive
/// `Deserialize`，故单独定义 DTO。
#[derive(Debug, Deserialize)]
struct InputSegmentWithAdjust {
    // —— base: OcrSegment ——（flatten SubtitleSegment：text/start_ms/end_ms）
    text: String,
    start_ms: u64,
    end_ms: u64,
    #[serde(default)]
    y_range: Option<[f32; 2]>,
    text_confidence: f32,
    #[serde(default)]
    frame_count: Option<u32>,
    #[serde(default)]
    frames: Option<Vec<serde_json::Value>>,
    // —— 调整附加字段 ——
    #[serde(default)]
    adjusted_text_confidence: Option<f32>,
    #[serde(default)]
    y_penalty: Option<f32>,
    #[serde(default)]
    iso_penalty: Option<f32>,
}

impl InputSegmentWithAdjust {
    fn into_ocr_segment_with_adjust(self) -> OcrSegmentWithAdjust {
        OcrSegmentWithAdjust {
            base: subtitle_ocr::OcrSegment {
                base: SubtitleSegment {
                    text: self.text,
                    start_ms: self.start_ms,
                    end_ms: self.end_ms,
                },
                y_range: self.y_range,
                text_confidence: self.text_confidence,
                frame_count: self.frame_count,
                frames: self.frames.map(|_| vec![]), // 帧明细不参与过滤，置空占位。
            },
            adjusted_text_confidence: self.adjusted_text_confidence,
            y_penalty: self.y_penalty,
            iso_penalty: self.iso_penalty,
        }
    }
}

/// 兼容两种输入形态：裸 `OcrSegmentWithAdjust[]` 数组，或
/// `OcrSegmentFilterResult { meta, result }`（filter 步骤的既有输出形状）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputSegments {
    Wrapped {
        #[serde(default)]
        segments: Vec<InputSegmentWithAdjust>,
    },
    Bare(Vec<InputSegmentWithAdjust>),
}

impl InputSegments {
    fn into_segments(self) -> Vec<OcrSegmentWithAdjust> {
        match self {
            InputSegments::Wrapped { segments } => segments
                .into_iter()
                .map(InputSegmentWithAdjust::into_ocr_segment_with_adjust)
                .collect(),
            InputSegments::Bare(segments) => segments
                .into_iter()
                .map(InputSegmentWithAdjust::into_ocr_segment_with_adjust)
                .collect(),
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "ocr-segment-filter",
    about = "字幕段置信度过滤：按调整后置信度阈值丢弃低置信段"
)]
struct Cli {
    /// 调整后字幕段 JSON 文件路径（裸 `OcrSegmentWithAdjust[]`，或 `ocr-segment-adjust` 的
    /// 输出；也可接 `OcrSegmentFilterResult { meta, result }`）。由 `ocr-segment-adjust` 产出。
    input: PathBuf,

    /// 置信度阈值：低于此值的段被丢弃。默认 0（不过滤）。
    #[arg(long, default_value_t = 0.0)]
    text_confidence_threshold: f32,

    /// 输出纯 `OcrSegmentWithAdjust[]` 数组（而非带 `meta`/`result` 的
    /// `OcrSegmentFilterResult`），便于回灌下游或再次 adjust。
    #[arg(long)]
    bare: bool,

    /// 把过滤结果额外写出到指定文件路径（同时仍向 stdout 打印）。便于落盘对接下游。
    #[arg(long)]
    out: Option<PathBuf>,
}

/// 仓库根：二进制在 `target/debug/ocr-segment-filter`，上溯两级到 workspace 根。
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
    let parsed: InputSegments = serde_json::from_str(&raw)
        .context("解析调整后 JSON 失败（需为 OcrSegmentWithAdjust[] 或 {meta,result}）")?;
    let segments = parsed.into_segments();

    // 默认输出带统计的 OcrSegmentFilterResult；--bare 则输出纯数组。
    if cli.bare {
        let filtered: Vec<OcrSegmentWithAdjust> =
            subtitle_ocr::ocr_segment_filter(&segments, cli.text_confidence_threshold);
        write_out(&repo_root, &cli.out, &filtered, filtered.len())?;
        println!("{}", serde_json::to_string_pretty(&filtered)?);
    } else {
        let result: OcrSegmentFilterResult =
            subtitle_ocr::ocr_segment_filter_with_meta(&segments, cli.text_confidence_threshold);
        write_out(&repo_root, &cli.out, &result, result.meta.segment_count)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}

/// 可选落盘：把 `value` 序列化后写到 `--out` 指定路径（已解析为绝对路径）。
fn write_out<T: serde::Serialize>(
    repo_root: &std::path::Path,
    out: &Option<PathBuf>,
    value: &T,
    segment_count: usize,
) -> Result<()> {
    if let Some(out) = out {
        let path = resolve_path(repo_root, out);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建输出目录失败: {}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(value).context("序列化过滤结果失败")?;
        std::fs::write(&path, json).with_context(|| format!("写入失败: {}", path.display()))?;
        info!(path = %path.display(), segments = segment_count, "已写出段");
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
