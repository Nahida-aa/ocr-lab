//! subtitle-finder CLI：对视频跑 FastSearchSubtitles，输出字幕关键帧 + 时间轴。
//!
//! 用法：
//!   cargo run -p subtitle-finder --release -- <video.mp4>
//!   cargo run -p subtitle-finder --release -- --profile <video.mp4>  # 附性能剖析
//!
//! 输出：
//!   - 关键帧 PNG 图（RGB，H×W）到 `out/` 目录，文件名 `{start_ms}_{end_ms}_{i}.png`
//!   - `out/timeline.txt`：每行 `start_ms,end_ms`（字幕段时间轴）
//!   - `out/keyframes.json`：结构化列表 { start_ms, end_ms, image }

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    // 初始化结构化日志（`RUST_LOG=subtitle_finder=debug` 可开调试日志）。
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        anyhow::bail!("用法: subtitle-finder [--profile] <video.mp4>");
    }
    let mut profile = false;
    let mut video_path: Option<String> = None;
    for a in &args[1..] {
        match a.as_str() {
            "--profile" => profile = true,
            _ => video_path = Some(a.clone()),
        }
    }
    let video_path = video_path.ok_or_else(|| anyhow::anyhow!("缺少视频路径"))?;
    let video = PathBuf::from(&video_path);
    if !video.exists() {
        anyhow::bail!("视频不存在: {}", video.display());
    }

    let params = subtitle_finder::params::Params::default();
    // 用带剖析的帧缓存。
    let mut cache = subtitle_finder::state::FrameCache::new(&video, &params);
    if profile {
        cache = cache.with_profiling();
    }
    let kfs = subtitle_finder::state::find_keyframes_with_cache(&mut cache, &params)?;

    if profile {
        if let Some(pf) = cache.profiler() {
            let n = cache.len();
            pf.dump(n);
        }
    }

    println!("找到 {} 个关键帧", kfs.len());

    // 输出目录：包内 out/（稳定、可查看）。
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out");
    std::fs::create_dir_all(&out_dir)?;

    let mut timeline = String::new();
    let mut json = Vec::new();
    for (i, kf) in kfs.iter().enumerate() {
        let name = format!("{}_{}_{}", kf.start_ms, kf.end_ms, i);
        println!("  [{}] {}ms - {}ms", i, kf.start_ms, kf.end_ms);

        // 保存原始帧 PNG（BGR → RGB，含背景）。
        let path = out_dir.join(format!("{}.png", name));
        save_png(&path, &kf.frame)?;
        // 保存去背景字幕前景 PNG（黑底白字，对应 VideoSubFinder 的 ISA 图）。
        let mask_path = out_dir.join(format!("{}_mask.png", name));
        save_mask_png(&mask_path, &kf.mask)?;

        // 时间轴。
        timeline.push_str(&format!("{},{}\n", kf.start_ms, kf.end_ms));
        json.push(format!(
            "{{\"start_ms\":{},\"end_ms\":{},\"image\":\"{}.png\",\"mask\":\"{}_mask.png\"}}",
            kf.start_ms, kf.end_ms, name, name
        ));
    }

    std::fs::write(out_dir.join("timeline.txt"), timeline)?;
    std::fs::write(
        out_dir.join("keyframes.json"),
        format!("[{}]\n", json.join(",")),
    )?;
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
