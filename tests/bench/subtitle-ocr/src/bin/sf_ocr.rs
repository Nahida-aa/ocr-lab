//! 跨包流程例子：subtitle-finder 提关键帧 → subtitle-ocr 识别 → 带时间轴字幕。
//!
//! 演示 `subtitle-finder`（吃视频出关键帧）与 `subtitle-ocr`（吃帧出字幕文本）
//! 两个独立包的协作，输出每段字幕的起止时间 + OCR 文本。
//!
//! 用法（仓库根）：
//!   cargo run -p bench-subtitle-ocr --bin sf_ocr -- <video.mp4>
//! 例：
//!   cargo run -p bench-subtitle-ocr --bin sf_ocr -- /tmp/clip5s.mp4
//!
//! 说明：
//! - subtitle-finder 的关键帧 = 每个字幕段的一张代表帧（原始 BGR，含背景）。
//! - 用 subtitle-ocr 的 `SubtitleOcr::ocr_image` 对代表帧识别文本（bottom_only 裁底部）。
//! - 关键帧自带 start_ms/end_ms 时间轴，直接拼成字幕行。
//! - 模型目录默认仓库根 `models/rapidocr`。

use std::path::{Path, PathBuf};

use anyhow::Result;
use rapidocr_ort::ModelProfile;
use subtitle_ocr::{OcrOptions, SubtitleOcr};

/// 仓库根：CARGO_MANIFEST_DIR = .../tests/bench/subtitle-ocr，上溯 3 级。
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // .../tests/bench
        .unwrap()
        .parent() // .../tests
        .unwrap()
        .parent() // 仓库根
        .unwrap()
        .to_path_buf()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        anyhow::bail!("用法: sf_ocr <video.mp4>");
    }
    let video = Path::new(&args[1]);
    if !video.exists() {
        anyhow::bail!("视频不存在: {}", video.display());
    }

    // 1) subtitle-finder 提关键帧（每个关键帧 = 一个字幕段）。
    let params = subtitle_finder::params::Params::default();
    let kfs = subtitle_finder::find_keyframes(video, &params)?;
    println!("找到 {} 个关键帧", kfs.len());

    // 2) 初始化 subtitle-ocr（模型目录仓库根 models/rapidocr）。
    let model_dir = repo_root().join("models").join("rapidocr");
    if !model_dir.exists() {
        anyhow::bail!("模型目录不存在: {}（用 --model 指定或放 models/rapidocr）", model_dir.display());
    }
    let mut ocr = SubtitleOcr::from_profile(ModelProfile::V3, &model_dir, OcrOptions::default())?;

    // 3) 对每个关键帧的原始帧 OCR，结合时间轴输出字幕。
    println!("---- 字幕时间轴 ----");
    for (i, kf) in kfs.iter().enumerate() {
        let lines = ocr.ocr_image(&kf.frame)?;
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let joined = text.join(" ");
        let t = if joined.is_empty() { "(未识别)".to_string() } else { joined };
        println!("[{}] {}-{}ms: {}", i, kf.start_ms, kf.end_ms, t);
    }

    Ok(())
}
