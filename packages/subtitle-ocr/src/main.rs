//! 命令行：`subtitle-ocr <image> [text_score]` 或 `subtitle-ocr --dir <dir> ...`
//!
//! 纯感知 OCR 工具，对标 cpp 的 `ocr_pipeline.cpp`：输出 JSON 数组，
//! 每个元素含 `text` / `confidence` / `boxes` / `timestamp`。
//!
//! 不含任何耗时字段——推理耗时是旁路观测数据，由调用方自行计时（CLI 在
//! `ocr_image` 调用前后 `Instant::now()` 测量，经 tracing 输出；benchmark 同理）。
//! 不污染 stdout 的 JSON 数组。
//!
//! 本 CLI 只做「逐图/批量 OCR」，不输出时间轴、不做帧合并——带时间戳的字幕段
//! 由知道视频结构的上游（自行补 `start`/`end` 后调用 `merge-frames`）负责。

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;
use std::path::{Path, PathBuf};
use subtitle_ocr::util::{BadNameAction, list_frames};
use subtitle_ocr::{OcrDevice, OcrEntry, OcrFramesMeta, OcrFramesResult, OcrOptions, SubtitleOcr};
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "subtitle-ocr",
    about = "字幕 OCR（基于 rapidocr-ort，PP-OCRv3）"
)]
struct Cli {
    /// 模型套件：v3 / v6-tiny / v6-medium
    #[arg(long, value_enum, default_value_t = rapidocr_ort::ModelProfile::V3)]
    model: rapidocr_ort::ModelProfile,

    /// 模型目录（默认仓库根 models/rapidocr）
    #[arg(long, default_value = "models/rapidocr")]
    model_dir: String,

    /// 输入图片路径（单图模式，不携带时间戳，输出 timestampMs=0；与 --dir 互斥）
    image: Option<String>,

    /// 批量模式：输入图片目录（jpg/jpeg/png/bmp，按文件名排序逐张识别，与 <image> 互斥）。
    ///
    /// 文件名须为 `ms` 或 `ms_ms` 形式，编码该图对应的时刻（毫秒），可前置多余 0：
    /// - `001234.png`        → 单时刻 1234
    /// - `001234_001250.png` → 双时刻 [1234, 1250]（同一张图仅识别一次，产出两个结果）
    /// 不符合格式时按 `--on-bad-name` 处理（`skip` 跳过 / `error` 报错）。
    #[arg(long)]
    dir: Option<String>,

    /// 批量模式（`--dir`）下，文件名不符合 `ms` / `ms_ms` 时间格式时的处理：
    /// `error` 直接报错终止（默认，避免静默丢帧）；`skip` 跳过并警告。
    #[arg(long, value_enum, default_value_t = BadNameAction::Error)]
    on_bad_name: BadNameAction,

    /// 识别置信度下限（cpp text_score，默认 0.5）
    #[arg(long)]
    text_score: Option<f32>,

    /// 仅保留画面底部比例区间的字幕框（cpp --subtitle-only）
    #[arg(long)]
    subtitle_only: bool,

    /// 关闭重叠框 NMS 去重（cpp --no-nms）
    #[arg(long)]
    no_nms: bool,

    /// 关闭 bottom_only：对整帧做 OCR（cpp 默认开启，故本包默认开启，此 flag 取反）
    #[arg(long)]
    full_frame: bool,

    /// 用 cpp 同款的透视矫正裁剪（warpPerspective）替代轴对齐包围盒。
    /// 与 det 几何 minAreaRect 耦合使用（实验对齐 cpp 用）。
    #[arg(long)]
    warp_crop: bool,

    /// 推理线程数（预留；当前 OcrEngine 固定 4，与 cpp 默认一致）
    #[arg(long)]
    threads: Option<usize>,

    /// 输出文件路径（完整文件名，由调用方决定，如 `asr_ocr_frames.json`）：写入
    /// `OcrFramesResult` 结构（各帧结果 + 溯源 meta），便于对接 LocalDub 的
    /// `asr_ocr_frames.json` / `sf_ocr_frames.json` 等。不指定时仅向 stdout 打印逐帧
    /// JSON 数组，行为与之前一致。两者可同时生效。
    #[arg(long)]
    out: Option<String>,
}

/// 仓库根：二进制在 `target/debug/subtitle-ocr`，上溯两级到 workspace 根。
fn current_exe_repo_root() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("获取当前可执行文件路径失败")?;
    let exe_dir = exe.parent().context("可执行文件无父目录")?.to_path_buf();
    let root = exe_dir
        .join("..") // 去掉 target/debug 或 target/release
        .join("..") // 去掉 packages/subtitle-ocr
        .canonicalize()
        .context("解析仓库根失败（确认从仓库内构建）")?;
    Ok(root)
}

fn resolve_path(repo_root: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

/// 单帧输出直接复用库里的 [`subtitle_ocr::FrameResult`]（文本 / 聚合置信度 /
/// 各框明细 / 几何值域 / 对应时刻），不再另定义输出结构。
///
/// 不含输入文件名——调用方本就知道自己喂了哪张图，批量模式下时刻信息已体现在
/// `timestamp`；也不含耗时字段——推理耗时是旁路观测数据，由调用方自行计时
/// （在 `ocr_image` 调用前后 `Instant::now()` 测量），不污染 JSON 结构。
fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let repo_root = current_exe_repo_root()?;
    let model_dir = resolve_path(&repo_root, &cli.model_dir);

    let opts = OcrOptions {
        bottom_only: !cli.full_frame,
        subtitle_only: cli.subtitle_only,
        use_nms: !cli.no_nms,
        text_score: cli.text_score.unwrap_or(0.5),
        use_warp_crop: cli.warp_crop,
    };

    let mut ocr = SubtitleOcr::from_profile(cli.model, &model_dir, opts)
        .context("构建字幕 OCR 引擎失败（确认 models/rapidocr 权重已就绪）")?;

    // 构建待识别条目：--dir 一张图可对应 1~2 个时刻（ms_ms 双时刻），
    // 单图 <image> 无时间。
    let entries: Vec<OcrEntry> = if let Some(dir) = &cli.dir {
        let dir = resolve_path(&repo_root, dir);
        list_frames(&dir, cli.on_bad_name)?
    } else if let Some(img) = &cli.image {
        vec![OcrEntry {
            path: resolve_path(&repo_root, img),
            times: subtitle_ocr::FrameTimes::None,
        }]
    } else {
        anyhow::bail!("必须提供 <image> 或 --dir <dir>");
    };

    // 核心流程（读图 → 识别 → 聚合 → 按时刻展开）复用库函数，避免各处照抄。
    let frame_outs: Vec<subtitle_ocr::FrameResult> = subtitle_ocr::ocr_entries(&mut ocr, &entries)?;

    // --out：额外落地 OcrFramesResult（文件名由调用方指定，如 asr_ocr_frames.json）
    if let Some(out) = &cli.out {
        let path = resolve_path(&repo_root, out);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建输出目录失败: {}", parent.display()))?;
            }
        }
        let result = OcrFramesResult {
            frames: frame_outs.clone(),
            meta: OcrFramesMeta {
                // 本包为 rust 实现；设备固定 cpu（cpp 侧才区分 cuda 等）。
                engine: "ort-rust".to_string(),
                device: OcrDevice::Cpu,
            },
        };
        let json = serde_json::to_string_pretty(&result).context("序列化 OcrFramesResult 失败")?;
        std::fs::write(&path, json).with_context(|| format!("写入失败: {}", path.display()))?;
        info!(path = %path.display(), frames = result.frames.len(), "已写出帧");
    }

    // 主输出：与 cpp 同形状的 JSON 数组（逐图/批量，不带时间轴）。
    let arr: Vec<Value> = frame_outs
        .iter()
        .map(|f| serde_json::to_value(f).unwrap_or(Value::Null))
        .collect();

    println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);

    Ok(())
}

/// 初始化 tracing subscriber：日志打到 stderr，级别由 `RUST_LOG` 环境变量控制
/// （默认 `warn`，即仅显示警告及以上；设 `info` 可看到写出进度等提示）。
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
}
