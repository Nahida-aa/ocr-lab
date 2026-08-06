//! 命令行：`subtitle-ocr <image> [text_score]` 或 `subtitle-ocr --dir <dir> ...`
//!
//! 对标 cpp 的 `ocr_pipeline.cpp`：输出与 cpp 同形状的 JSON 数组，每个元素含
//! `file` / `text` / `segments` / `detInferenceMs` / `postprocessMs` /
//! `recInferenceMs` / `totalMs`。`--merge` 时额外在 `--dir` 模式输出合并后的
//! 带时间轴字幕段（供 bench 的 `load_timed_segments` 消费）。

use anyhow::{Context, Result};
use clap::Parser;
use ndarray::Array3;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use subtitle_ocr::{FrameResult, MergeArgs, OcrOptions, SubtitleOcr};

#[derive(Parser, Debug)]
#[command(name = "subtitle-ocr", about = "字幕 OCR（基于 rapidocr-ort，PP-OCRv3）")]
struct Cli {
    /// 模型套件：v3 / v6-tiny / v6-medium
    #[arg(long, value_enum, default_value_t = rapidocr_ort::ModelProfile::V3)]
    model: rapidocr_ort::ModelProfile,

    /// 模型目录（默认仓库根 models/rapidocr）
    #[arg(long, default_value = "models/rapidocr")]
    model_dir: String,

    /// 输入图片路径（与 --dir 互斥）
    image: Option<String>,

    /// 批量模式：目录（列出 jpg/jpeg/png/bmp，排序）
    #[arg(long)]
    dir: Option<String>,

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

    /// 在 --dir 模式下额外输出合并后的带时间轴字幕段（需配合 --fps）
    #[arg(long)]
    merge: bool,

    /// 抽帧帧率（仅 --merge 用于计算时间戳，默认 2）
    #[arg(long, default_value_t = 2.0)]
    fps: f64,
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

/// 读图为 BGR HWC u8 的 `Array3`。
///
/// 用 BGR 而非 RGB：PP-OCR/rapidocr 模型按 `cv2.imread`（BGR）训练（cpp 同款）。
/// 用 RGB 会让彩色字幕（如本视频的 '啊'）出现漏检/误识，故这里统一转 BGR 对齐。
fn load_rgb(path: &Path) -> Result<Array3<u8>> {
    let img = image::open(path)
        .with_context(|| format!("读取图片失败: {}", path.display()))?
        .to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut data = img.into_raw();
    // RGB→BGR：image crate 给 RGB，模型要 BGR。
    for px in data.chunks_mut(3) {
        px.swap(0, 2);
    }
    Array3::from_shape_vec((h, w, 3), data).context("图像数据重塑失败（维度不匹配）")
}

/// 列出目录下图片文件并排序（对齐 cpp listFrames）。
fn list_frames(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                let ext = ext.to_lowercase();
                if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp") {
                    files.push(p);
                }
            }
        }
    }
    files.sort();
    files
}

/// 单帧输出（对齐 cpp toJson 的形状）。
#[derive(Serialize)]
struct FrameOut {
    #[serde(rename = "file")]
    file: String,
    text: String,
    segments: Vec<SegOut>,
    #[serde(rename = "detInferenceMs")]
    det_inference_ms: f64,
    #[serde(rename = "postprocessMs")]
    postprocess_ms: f64,
    #[serde(rename = "recInferenceMs")]
    rec_inference_ms: f64,
    #[serde(rename = "totalMs")]
    total_ms: f64,
}

#[derive(Serialize)]
struct SegOut {
    text: String,
    confidence: f64,
    #[serde(rename = "box")]
    box_: Vec<[f32; 2]>,
}

fn to_seg_out(line: &subtitle_ocr::FrameLine) -> SegOut {
    SegOut {
        text: line.text.clone(),
        confidence: line.confidence as f64,
        box_: line.box_.iter().map(|p| [p[0], p[1]]).collect(),
    }
}

fn main() -> Result<()> {
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

    if cli.merge && cli.dir.is_none() {
        anyhow::bail!("--merge 仅支持与 --dir 批量模式配合使用");
    }

    // 构建帧路径列表。
    let frame_paths: Vec<PathBuf> = if let Some(dir) = &cli.dir {
        let dir = resolve_path(&repo_root, dir);
        list_frames(&dir)
    } else if let Some(img) = &cli.image {
        vec![resolve_path(&repo_root, img)]
    } else {
        anyhow::bail!("必须提供 <image> 或 --dir <dir>");
    };

    let mut frame_outs: Vec<FrameOut> = Vec::with_capacity(frame_paths.len());
    let mut timed_frames: Vec<FrameResult> = Vec::with_capacity(frame_paths.len());

    for (idx, fp) in frame_paths.iter().enumerate() {
        let rgb = load_rgb(fp)?;
        let (lines, det_ms) = ocr.ocr_image_timed(&rgb)?;
        let text: String = lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("");

        let seg_outs: Vec<SegOut> = lines.iter().map(to_seg_out).collect();

        frame_outs.push(FrameOut {
            file: fp.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            text,
            segments: seg_outs,
            det_inference_ms: det_ms,
            postprocess_ms: 0.0, // rapidocr-ort detect 内部不单独计时
            rec_inference_ms: 0.0,
            total_ms: det_ms,
        });

        if cli.merge {
            let ts = subtitle_ocr::frame_timestamp_ms(idx, cli.fps);
            let fr = ocr.aggregate_frame(&lines, ts);
            timed_frames.push(fr);
        }
    }

    // 主输出：与 cpp 同形状的 JSON 数组。
    let arr: Vec<Value> = frame_outs
        .iter()
        .map(|f| {
            json!({
                "file": f.file,
                "text": f.text,
                "segments": f.segments.iter().map(|s| json!({
                    "text": s.text,
                    "confidence": s.confidence,
                    "box": s.box_,
                })).collect::<Vec<_>>(),
                "detInferenceMs": f.det_inference_ms,
                "postprocessMs": f.postprocess_ms,
                "recInferenceMs": f.rec_inference_ms,
                "totalMs": f.total_ms,
            })
        })
        .collect();

    if cli.merge {
        let segments = subtitle_ocr::merge_frames(&timed_frames, &MergeArgs::default());
        let merged: Vec<Value> = segments
            .iter()
            .map(|s| {
                json!({
                    "text": s.text,
                    "start": s.start_ms,
                    "end": s.end_ms,
                    "confidence": s.confidence,
                    "x_range": s.x_range,
                    "y_range": s.y_range,
                })
            })
            .collect();
        // 把 merged 段挂在一个顶层对象里，与 frame 数组分离，避免破坏 cpp 解析。
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "frames": arr,
                "segments": merged,
            }))?
        );
    } else {
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);
    }

    Ok(())
}
