//! subtitle-finder CLI：对视频跑 FastSearchSubtitles，输出字幕关键帧 + 时间轴。
//!
//! 用法：
//!   cargo run -p subtitle-finder --release -- <video.mp4>
//!   cargo run -p subtitle-finder --release -- --profile <video.mp4>  # 附性能剖析
//!   cargo run -p subtitle-finder --release -- --output timeline <video.mp4>  # 只写 timeline.txt
//!   cargo run -p subtitle-finder --release -- --output frames <video.mp4>    # 仅原始关键帧 PNG
//!
//! 输出（由 `--output` 控制，均落在 `--out` 目录下）：
//!   - `full`（默认）：原始关键帧 PNG 在 `frames/`（`{start_ms}_{end_ms}.png`，
//!     与 subtitle-ocr `--dir` 的 `ms_ms` 时间区间约定一致，可直接喂下游 OCR）、
//!     去背景掩码 PNG 在 `mask/`（同名）、`timeline.txt`（每行 `start_ms,end_ms`）、
//!     `keyframes.json`（结构化列表，image/mask 字段为裸文件名）。
//!   - `timeline`：只写 `timeline.txt`，跳过 PNG / keyframes.json（纯算法计时用）。
//!   - `frames`：仅写 `frames/` 下原始关键帧 PNG（含背景），不写掩码 / json / timeline。

use anyhow::Context;
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use tracing::trace;

/// 输出模式：控制落盘哪些产物。
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputMode {
    /// 写关键帧 PNG + 掩码 PNG + timeline.txt + keyframes.json（默认）。
    Full,
    /// 只写 timeline.txt（纯算法计时，不落盘图片）。
    Timeline,
    /// 仅写原始关键帧 PNG（RGB，含背景），不写掩码 / json / timeline。
    Frames,
}

#[derive(Parser, Debug)]
#[command(name = "subtitle-finder", about = "对视频跑字幕关键帧筛选，输出关键帧 + 时间轴")]
struct Cli {
    /// 附性能剖析（分阶段耗时统计）。
    #[arg(long)]
    profile: bool,

    /// 输出模式：full=全量产物，timeline=只写时间轴，frames=仅原始关键帧 PNG。
    #[arg(long, value_enum, default_value_t = OutputMode::Full)]
    output: OutputMode,

    /// 输出目录（相对仓库根；默认包内 out/）。
    #[arg(long)]
    out: Option<String>,

    /// 输入视频路径（相对仓库根，或绝对路径）。
    #[arg(required = true)]
    video: String,
}

/// 仓库根：二进制在 `target/release/subtitle-finder`，上溯两级到 workspace 根。
fn repo_root() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("获取当前可执行文件路径失败")?;
    let exe_dir = exe.parent().context("可执行文件无父目录")?.to_path_buf();
    let root = exe_dir
        .join("..") // 去掉 target/release
        .join("..") // 去掉 packages/subtitle-finder
        .canonicalize()
        .context("解析仓库根失败（确认从仓库内构建）")?;
    Ok(root)
}

/// 相对路径相对仓库根解析；绝对路径原样返回。
fn resolve_path(root: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn main() -> anyhow::Result<()> {
    // 初始化结构化日志（`RUST_LOG=subtitle_finder=debug` 可开调试日志）。
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let profile = cli.profile;
    // 各产物是否落盘（由 --output 决定）。
    let write_frames = cli.output == OutputMode::Full || cli.output == OutputMode::Frames;
    let write_mask = cli.output == OutputMode::Full;
    let write_json = cli.output == OutputMode::Full;
    let write_timeline = cli.output == OutputMode::Full || cli.output == OutputMode::Timeline;

    // 相对路径相对仓库根解析（与 subtitle-ocr 一致：二进制在 target/ 下，
    // 上溯两级即仓库根），不依赖调用方当前工作目录。
    let repo_root = repo_root()?;
    let video = resolve_path(&repo_root, &cli.video);
    if !video.exists() {
        anyhow::bail!("视频不存在: {}", video.display());
    }

    let params = subtitle_finder::params::Params::default();
    // 用带剖析的帧缓存。
    let mut cache = subtitle_finder::state::FrameCache::new(&video, &params);
    if profile {
        cache = cache.with_profiling();
    }
    // 强制打开解码器（惰性），使 total_frames() 在进度条建条前已就绪。
    cache.advance_to(0)?;

    // 解码阶段进度条（按帧数，走 stderr；不依赖 duration，始终有总量）。
    // total==0（nb_frames 与 duration×fps 皆不可得）时退化为无总量进度。
    let total_frames = cache.total_frames().max(0) as u64;
    let pb = ProgressBar::new(total_frames);
    if total_frames == 0 {
        // 未知总帧数：隐藏长度，仅显示已解码帧数 + 耗时。
        pb.set_style(
            ProgressStyle::with_template("[{elapsed_precise}] 解码 {pos} 帧")
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
    } else {
        pb.set_style(
            ProgressStyle::with_template(
                "[{elapsed_precise}] 解码 [{bar:30.cyan/blue}] {pos}/{len} 帧 ({eta})",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
        );
    }
    let kfs = subtitle_finder::state::find_keyframes_with_cache_progress(
        &mut cache,
        &params,
        &mut |decoded, _total| {
            pb.set_position(decoded);
        },
    )?;
    // 保留进度条满格那一行（不消失，对齐 subtitle-ocr 的 finish 行为）；
    // 耗时用 eprintln 另起一行（stderr 直写一定落盘，便于事后回看总耗时）。
    let decode_elapsed = pb.elapsed();
    pb.finish();
    eprintln!("解码完成 {} 帧，耗时 {:.1}s", cache.len(), decode_elapsed.as_secs_f64());

    if profile {
        if let Some(pf) = cache.profiler() {
            let n = cache.len();
            pf.dump(n);
        }
    }

    println!("找到 {} 个关键帧", kfs.len());

    // 输出目录：默认包内 out/（相对 manifest 目录）；--out 指定则相对仓库根解析。
    let out_dir = match &cli.out {
        Some(d) => resolve_path(&repo_root, d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out"),
    };
    // 原始关键帧放 frames/，去背景掩码放 mask/（分目录，无需后缀区分，
    // subtitle-ocr --dir frames/ 直接可用）。
    let frames_dir = out_dir.join("frames");
    let mask_dir = out_dir.join("mask");
    std::fs::create_dir_all(&frames_dir)?;
    if write_mask {
        std::fs::create_dir_all(&mask_dir)?;
    }

    let mut timeline = String::new();
    let mut json = Vec::new();
    // 落盘进度条（按关键帧数）。逐帧明细降级为 trace!（默认 info 不过滤，
    // 避免 per-frame println 的终端 flush 抖动 + 与 stderr 进度条混用）。
    let save_pb = ProgressBar::new(kfs.len() as u64);
    save_pb.set_style(
        ProgressStyle::with_template("[{elapsed_precise}] 落盘 [{bar:30.green}] {pos}/{len}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    for (i, kf) in kfs.iter().enumerate() {
        // 文件名用 `start_ms_end_ms`（`ms_ms` 时间区间格式），与 subtitle-ocr
        // 的 `--dir` 批量模式约定一致，可直接喂下游 OCR。关键帧时间区间互不
        // 重叠，start_ms_end_ms 已天然唯一，无需再附加帧序号。
        let name = format!("{}_{}", kf.start_ms, kf.end_ms);
        trace!("  [{}] {}ms - {}ms", i, kf.start_ms, kf.end_ms);

        if write_frames {
            // 保存原始帧 PNG（BGR → RGB，含背景）→ frames/。
            let path = frames_dir.join(format!("{}.png", name));
            save_png(&path, &kf.frame)?;
        }
        if write_mask {
            // 保存去背景字幕前景 PNG（黑底白字，对应 VideoSubFinder 的 ISA 图）→ mask/。
            let mask_path = mask_dir.join(format!("{}.png", name));
            save_mask_png(&mask_path, &kf.mask)?;

            json.push(format!(
                "{{\"start_ms\":{},\"end_ms\":{},\"image\":\"{}.png\",\"mask\":\"{}.png\"}}",
                kf.start_ms, kf.end_ms, name, name
            ));
        }

        if write_timeline {
            timeline.push_str(&format!("{},{}\n", kf.start_ms, kf.end_ms));
        }
        save_pb.inc(1);
    }
    let save_elapsed = save_pb.elapsed();
    save_pb.finish();
    eprintln!("落盘完成 {} 张，耗时 {:.1}s", kfs.len(), save_elapsed.as_secs_f64());

    if write_timeline {
        std::fs::write(out_dir.join("timeline.txt"), timeline)?;
    }
    if write_json {
        std::fs::write(
            out_dir.join("keyframes.json"),
            format!("[{}]\n", json.join(",")),
        )?;
    }
    println!("输出目录: {}", out_dir.display());
    Ok(())
}

/// 把 BGR `Array3`（H×W×3）存为 PNG（转 RGB）。
fn save_png(path: &std::path::Path, arr: &ndarray::Array3<u8>) -> anyhow::Result<()> {
    let (h, w, _) = arr.dim();
    let mut rgb = Vec::with_capacity(h * w * 3);
    for y in 0..h {
        for x in 0..w {
            // ndarray 存 BGR → PNG 要 RGB。
            rgb.push(arr[[y, x, 2]]); // R
            rgb.push(arr[[y, x, 1]]); // G
            rgb.push(arr[[y, x, 0]]); // B
        }
    }
    let img = image::RgbImage::from_raw(w as u32, h as u32, rgb)
        .ok_or_else(|| anyhow::anyhow!("构造 RgbImage 失败"))?;
    img.save(path)?;
    Ok(())
}

/// 把字幕前景 mask `Array2`（H×W，255=文字）存为 PNG（黑底白字）。
fn save_mask_png(path: &std::path::Path, mask: &ndarray::Array2<u8>) -> anyhow::Result<()> {
    let (h, w) = mask.dim();
    // 直接作为灰度图（0=黑背景，255=白字幕）。
    let gray = image::GrayImage::from_raw(w as u32, h as u32, mask.iter().copied().collect())
        .ok_or_else(|| anyhow::anyhow!("构造 GrayImage 失败"))?;
    gray.save(path)?;
    Ok(())
}
