//! subtitle-ocr 性能基准（准确性 + 速度）。
//!
//! 移植自 LocalDub packages/benchmark/ocr/compute/benchmark-ocr-video.ts，
//! 用于精确复刻 `--engine cpp --only ocr-cpp-fps2-so-ts0.45` 路径以对比结果。
//!
//! 用法：
//!   cargo run -p bench-subtitle-ocr --bin bench -- --impl cpp [--fps 2] [--text-score 0.45] [--subtitle-only]
//!   cargo run -p bench-subtitle-ocr --bin bench -- --impl cpp --only ocr-cpp-fps2-so-ts0.45
//!
//! 当前精确复刻 cpp 单帧逐帧调用路径；py/rust 未实现时跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use bench_subtitle_ocr::{
    align_segments, extract_frames, list_frame_files, merge_frames, normalize_for_cer, compute_cer,
    AlignReport, FrameResult, TimedText,
};

fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn cpp_bin() -> PathBuf {
    repo_root()
        .join("packages")
        .join("subtitle-ocr-cpp")
        .join("build")
        .join("subtitle_ocr_ort_cpp")
}

fn models_dir() -> PathBuf {
    repo_root().join("models").join("rapidocr")
}

fn video_path() -> PathBuf {
    repo_root()
        .join("tests")
        .join("bench")
        .join("subtitle-ocr")
        .join("ref")
        .join("video_source.mp4")
}

fn gt_path() -> PathBuf {
    repo_root()
        .join("tests")
        .join("bench")
        .join("subtitle-ocr")
        .join("ref")
        .join("ocr_manual.json")
}

fn tmp_dir() -> PathBuf {
    repo_root().join("packages").join("tmp").join("ocr-bench")
}

/// 对齐 ocrFrameCpp：单帧模式调用 cpp 二进制，取 segments 里最高 confidence 的
/// 一个作为该帧文本（丢弃 box，对齐原版行为）。
#[derive(Default)]
struct CppFrame {
    text: String,
    confidence: f64,
    total_ms: f64,
    det_ms: f64,
    post_ms: f64,
    rec_ms: f64,
}

fn ocr_frame_cpp(
    frame: &Path,
    text_score: Option<f64>,
    subtitle_only: bool,
    threads: Option<usize>,
) -> CppFrame {
    let bin = cpp_bin();
    let md = models_dir();
    let mut args: Vec<String> = vec![frame.to_str().unwrap().to_string()];
    if let Some(ts) = text_score {
        args.push(ts.to_string());
    }
    if subtitle_only {
        args.push("--subtitle-only".to_string());
    }
    if let Some(n) = threads {
        args.push("--threads".to_string());
        args.push(n.to_string());
    }
    let out = Command::new(&bin)
        .args(&args)
        .env("OCR_MODELS_DIR", &md)
        .env("OCR_KEYS_PATH", md.join("ppocr_keys.json"))
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let parsed: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null);
            let data = if parsed.is_array() {
                parsed.get(0).cloned().unwrap_or(serde_json::Value::Null)
            } else {
                parsed
            };
            let segs = data.get("segments").and_then(|s| s.as_array()).cloned().unwrap_or_default();
            let best = segs
                .iter()
                .filter_map(|s| {
                    let t = s.get("text").and_then(|x| x.as_str())?.to_string();
                    let c = s.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    Some((t, c))
                })
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let (text, confidence) = best.unwrap_or_default();
            // 纯推理耗时 = det + post + rec，排除模型/字符表加载（单帧模式每进程
            // 重加载，若计入 totalMs 会把 RTF 放大 N 倍，无法与 --dir 批量模式横比）。
            let det = data.get("detInferenceMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let post = data.get("postprocessMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let rec = data.get("recInferenceMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
            CppFrame {
                text,
                confidence,
                total_ms: det + post + rec,
                det_ms: det,
                post_ms: post,
                rec_ms: rec,
            }
        }
        _ => {
            eprintln!("  cpp OCR 调用失败: {}", frame.display());
            CppFrame::default()
        }
    }
}

/// --dir 批量模式：一次性喂整个帧目录，返回每帧的 best segment（与单帧模式对称）。
/// 对齐原版 runOCRBenchmarkCppOpencv 的调用方式。
fn ocr_dir_cpp(
    frame_dir: &Path,
    text_score: Option<f64>,
    subtitle_only: bool,
    threads: Option<usize>,
) -> Vec<CppFrame> {
    let bin = cpp_bin();
    let md = models_dir();
    let mut args: Vec<String> = vec!["--dir".to_string(), frame_dir.to_str().unwrap().to_string()];
    if let Some(ts) = text_score {
        args.push(ts.to_string());
    }
    if subtitle_only {
        args.push("--subtitle-only".to_string());
    }
    if let Some(n) = threads {
        args.push("--threads".to_string());
        args.push(n.to_string());
    }
    let out = Command::new(&bin)
        .args(&args)
        .env("OCR_MODELS_DIR", &md)
        .env("OCR_KEYS_PATH", md.join("ppocr_keys.json"))
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let parsed: serde_json::Value =
                serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null);
            let arr = parsed.as_array().cloned().unwrap_or_default();
            arr.iter()
                .map(|data| {
                    let segs = data.get("segments").and_then(|s| s.as_array()).cloned().unwrap_or_default();
                    let best = segs
                        .iter()
                        .filter_map(|s| {
                            let t = s.get("text").and_then(|x| x.as_str())?.to_string();
                            let c = s.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0);
                            Some((t, c))
                        })
                        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                    let (text, confidence) = best.unwrap_or_default();
                    let det = data.get("detInferenceMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let post = data.get("postprocessMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let rec = data.get("recInferenceMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    CppFrame {
                        text,
                        confidence,
                        total_ms: det + post + rec,
                        det_ms: det,
                        post_ms: post,
                        rec_ms: rec,
                    }
                })
                .collect()
        }
        _ => {
            eprintln!("  cpp --dir OCR 调用失败: {}", frame_dir.display());
            Vec::new()
        }
    }
}

#[derive(Default)]
struct Summary {
    label: String,
    fps: f64,
    engine: String,
    frames: usize,
    segments: usize,
    audio_duration_s: f64,
    ocr_inference_s: f64,
    ocr_rtf: f64,
    cer_raw: f64,
    cer_norm: f64,
    hyp_chars: usize,
    ref_chars: usize,
    text_score: f64,
    subtitle_only: bool,
    use_dir: bool,
    threads: Option<usize>,
    timings: Option<Timings>,
}

struct Timings {
    total: f64,
    avg_per_frame: f64,
    det: f64,
    post: f64,
    rec: f64,
}

/// rust 实现二进制路径（与 cpp 对称，在 packages/subtitle-ocr 下构建）。
/// 优先用 release 构建（与 cpp 的 release 二进制公平对比；debug 二进制因未优化
/// opencv/ort 会慢一个数量级，RTF 不可比）。
fn rust_bin() -> PathBuf {
    let root = repo_root();
    let release = root.join("target").join("release").join("subtitle-ocr");
    if release.exists() {
        release
    } else {
        root.join("target").join("debug").join("subtitle-ocr")
    }
}

/// 对齐 ocrFrameCpp：单帧模式调用 rust 二进制（subtitle-ocr），取 segments 里最高
/// confidence 的一个作为该帧文本（丢弃 box，与 cpp 路径行为一致）。
fn ocr_frame_rust(
    frame: &Path,
    text_score: Option<f64>,
    subtitle_only: bool,
    _threads: Option<usize>,
) -> CppFrame {
    let bin = rust_bin();
    let mut args: Vec<String> = vec![frame.to_str().unwrap().to_string()];
    if let Some(ts) = text_score {
        args.push("--text-score".to_string());
        args.push(ts.to_string());
    }
    if subtitle_only {
        args.push("--subtitle-only".to_string());
    }
    let out = Command::new(&bin).args(&args).output();
    // 单帧模式：rust 仍输出单元素数组，取首个。
    parse_rust_json(out, "single-frame")
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// 对齐 ocrDirCpp：批量模式调用 rust 二进制 `--dir`，返回与 cpp 同形状的帧结果。
fn ocr_dir_rust(
    frame_dir: &Path,
    text_score: Option<f64>,
    subtitle_only: bool,
    _threads: Option<usize>,
) -> Vec<CppFrame> {
    let bin = rust_bin();
    let mut args: Vec<String> = vec!["--dir".to_string(), frame_dir.to_str().unwrap().to_string()];
    if let Some(ts) = text_score {
        args.push("--text-score".to_string());
        args.push(ts.to_string());
    }
    if subtitle_only {
        args.push("--subtitle-only".to_string());
    }
    let out = Command::new(&bin).args(&args).output();
    parse_rust_json(out, "dir")
}

/// 解析 rust 二进制输出（JSON 数组，每元素含 segments / detInferenceMs 等），
/// 形状与 cpp 一致，故直接复用 CppFrame。
fn parse_rust_json(out: Result<std::process::Output, std::io::Error>, mode: &str) -> Vec<CppFrame> {
    match out {
        Ok(o) if o.status.success() => {
            let parsed: serde_json::Value =
                serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null);
            // --dir 模式下为数组；单帧模式也是单元素数组（与 cpp 对齐）。
            let arr = parsed.as_array().cloned().unwrap_or_default();
            arr.iter()
                .map(|data| {
                    let segs = data
                        .get("segments")
                        .and_then(|s| s.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let best = segs
                        .iter()
                        .filter_map(|s| {
                            let t = s.get("text").and_then(|x| x.as_str())?.to_string();
                            let c = s.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0);
                            Some((t, c))
                        })
                        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                    let (text, confidence) = best.unwrap_or_default();
                    // rust 的 det/rec 在 rapidocr-ort detect 内 fused：整段 detect 计为
                    // detInferenceMs，post/rec 记 0，三者之和即 cpp 的 totalMs 口径。
                    let det = data.get("detInferenceMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let post = data.get("postprocessMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let rec = data.get("recInferenceMs").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    CppFrame {
                        text,
                        confidence,
                        total_ms: det + post + rec,
                        det_ms: det,
                        post_ms: post,
                        rec_ms: rec,
                    }
                })
                .collect()
        }
        _ => {
            eprintln!("  rust subtitle-ocr 调用失败 ({} 模式)", mode);
            Vec::new()
        }
    }
}

/// 对齐 runBenchmarkCommon 的 cpp 路径（精确复刻 --only ocr-cpp-fps2-so-ts0.45）。
/// use_dir=true 时用 --dir 批量模式（对齐 runOCRBenchmarkCppOpencv），否则逐帧单帧
/// （对齐 runOCRBenchmarkCpp）。两种模式产出相同的 frame_results。
fn run_benchmark_cpp(
    label: &str,
    fps: f64,
    text_score: f64,
    subtitle_only: bool,
    use_dir: bool,
    threads: Option<usize>,
) {
    let vpath = video_path();
    let out_dir = tmp_dir().join(format!("frames-{}", label));
    std::fs::create_dir_all(&out_dir).unwrap();

    println!("\n=== OCR Benchmark: {} (fps={}, engine=ort-cpp, mode={}, threads={}) ===",
        label, fps, if use_dir { "--dir" } else { "single-frame" },
        threads.map(|n| n.to_string()).unwrap_or_else(|| "default".into()));
    println!("  extracting frames...");
    let (duration_s, step, src_fps) = extract_frames(&vpath, &out_dir, fps);
    let frame_files = list_frame_files(&out_dir);

    // 调用 cpp：--dir 批量 or 逐帧单帧（由 use_dir 控制，而非拆成两个 runner）
    let mut frame_results: Vec<FrameResult> = Vec::new();
    let mut total_ms = 0.0f64;
    let mut total_det = 0.0f64;
    let mut total_post = 0.0f64;
    let mut total_rec = 0.0f64;

    if use_dir {
        // 批量模式：一次喂整个目录，按其返回顺序对应帧文件
        let results = ocr_dir_cpp(&out_dir, Some(text_score), subtitle_only, threads);
        for (i, r) in results.into_iter().enumerate() {
            let timestamp = (((i as f64) * (step as f64)) / src_fps * 1000.0).round() as u64;
            frame_results.push(FrameResult {
                text: r.text.clone(),
                timestamp,
                confidence: r.confidence,
                bbox: None,
            });
            total_ms += r.total_ms;
            total_det += r.det_ms;
            total_post += r.post_ms;
            total_rec += r.rec_ms;
        }
    } else {
        // 逐帧单帧模式（对齐 ocrFrameCpp）
        for (i, f) in frame_files.iter().enumerate() {
            let r = ocr_frame_cpp(f, Some(text_score), subtitle_only, threads);
            let timestamp = (((i as f64) * (step as f64)) / src_fps * 1000.0).round() as u64;
            frame_results.push(FrameResult {
                text: r.text.clone(),
                timestamp,
                confidence: r.confidence,
                bbox: None, // 对齐 ocrFrameCpp 丢弃 box
            });
            total_ms += r.total_ms;
            total_det += r.det_ms;
            total_post += r.post_ms;
            total_rec += r.rec_ms;
        }
    }

    // 合并帧（对齐 mergeFrames(frameResults, {})）
    let (merged_text, segments) = merge_frames(&frame_results);
    let inference_s = total_ms / 1000.0;
    let rtf = if duration_s > 0.0 {
        inference_s / duration_s
    } else {
        0.0
    };
    let has_timings = total_det > 0.0 || total_post > 0.0 || total_rec > 0.0;

    // 写 ocr.json（对齐 runBenchmarkCommon 的 ocrOutput 结构）
    let ocr_output = serde_json::json!({
        "audio_info": {
            "duration": if segments.is_empty() { (duration_s * 1000.0).round() as u64 }
                        else { segments.last().unwrap().end }
        },
        "result": {
            "text": merged_text,
            "segments": segments.iter().map(|s| serde_json::json!({
                "text": s.text,
                "start": s.start,
                "end": s.end,
                "confidence": s.confidence,
            })).collect::<Vec<_>>(),
        },
        "_engine": "ort-cpp",
        "_source": "video_hardsub",
        "_fps": fps,
        "_textScore": text_score,
        "_subtitleOnly": subtitle_only,
        "_timingsMs": if has_timings {
            serde_json::json!({
                "total": total_ms.round() as u64,
                "averagePerFrame": (total_ms / frame_results.len().max(1) as f64).round() as u64,
                "det": total_det.round() as u64,
                "post": total_post.round() as u64,
                "rec": total_rec.round() as u64,
            })
        } else {
            serde_json::Value::Null
        },
    });

    let metadata_dir = tmp_dir().join(label).join("metadata");
    std::fs::create_dir_all(&metadata_dir).unwrap();
    let ocr_path = metadata_dir.join("ocr.json");
    std::fs::write(&ocr_path, serde_json::to_string_pretty(&ocr_output).unwrap()).unwrap();

    // 算 CER（对齐 runEvalOCR：读 ocr.json 的 result.segments，join 文本，比 GT）
    let cer = eval_cer(&ocr_path, &gt_path());
    // 时序对齐质量：拼串 CER 丢掉了时间戳，这里按时间重叠配对后另算一组指标
    let align = align_segments(
        &load_timed_segments(&gt_path()),
        &load_timed_segments(&ocr_path),
    );

    let summary = Summary {
        label: label.to_string(),
        fps,
        engine: "ort-cpp".to_string(),
        frames: frame_results.len(),
        segments: segments.len(),
        audio_duration_s: (duration_s * 10.0).round() / 10.0,
        ocr_inference_s: (inference_s * 1000.0).round() / 1000.0,
        ocr_rtf: (rtf * 10000.0).round() / 10000.0,
        cer_raw: cer.raw,
        cer_norm: cer.norm,
        hyp_chars: cer.hyp_chars,
        ref_chars: cer.ref_chars,
        text_score,
        subtitle_only,
        use_dir,
        threads,
        timings: if has_timings {
            Some(Timings {
                total: total_ms,
                avg_per_frame: total_ms / frame_results.len().max(1) as f64,
                det: total_det,
                post: total_post,
                rec: total_rec,
            })
        } else {
            None
        },
    };

    // 写 summary.json
    let summary_json = serde_json::json!({
        "label": summary.label,
        "fps": summary.fps,
        "engine": summary.engine,
        "frames": summary.frames,
        "segments": summary.segments,
        "audio_duration_s": summary.audio_duration_s,
        "ocr_inference_s": summary.ocr_inference_s,
        "ocr_rtf": summary.ocr_rtf,
        "wer": summary.cer_norm,
        "cer": summary.cer_norm,
        "hyp_chars": summary.hyp_chars,
        "ref_chars": summary.ref_chars,
        "textScore": summary.text_score,
        "subtitleOnly": summary.subtitle_only,
        // 复现所需的运行配置：批量/单帧模式与 ORT 线程数（null = cpp 侧默认）
        "mode": if summary.use_dir { "dir" } else { "single-frame" },
        "intraOpThreads": summary.threads,
        // 时序对齐质量（按时间重叠配对；delta 正值 = hyp 偏晚，单位 ms）
        "alignment": {
            "paired": align.pairs.len(),
            "missed": align.missed,
            "spurious": align.spurious,
            "zero_duration": align.zero_duration,
            "split": align.split,
            "merged": align.merged,
            "iou_mean": (align.iou_mean * 10000.0).round() / 10000.0,
            "paired_cer": align.paired_cer,
            "start_delta_ms": {
                "mean": align.start_delta_mean.round(),
                "median": align.start_delta_median.round(),
                "p95_abs": align.start_delta_p95_abs.round(),
            },
            "end_delta_ms": {
                "mean": align.end_delta_mean.round(),
                "median": align.end_delta_median.round(),
                "p95_abs": align.end_delta_p95_abs.round(),
            },
        },
        "ocr_timings_ms": summary.timings.as_ref().map(|t| serde_json::json!({
            "total": t.total.round() as u64,
            "avgPerFrame": t.avg_per_frame.round() as u64,
            "det": t.det.round() as u64,
            "post": t.post.round() as u64,
            "rec": t.rec.round() as u64,
        })),
    });
    std::fs::write(
        metadata_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary_json).unwrap(),
    )
    .unwrap();

    // 控制台报告（对齐 benchmark-ocr-video.ts 的 log）
    println!(
        "  merged {} frames → {} segments",
        frame_results.len(),
        segments.len()
    );
    println!("  segs={} dur={:.1}s", segments.len(), duration_s);
    if has_timings {
        println!(
            "  det={}ms post={}ms rec={}ms total={}ms",
            total_det.round() as u64,
            total_post.round() as u64,
            total_rec.round() as u64,
            total_ms.round() as u64
        );
        println!(
            "  avg/frame={}ms  inference={:.3}s RTF={:.4}",
            (total_ms / frame_results.len().max(1) as f64).round() as u64,
            inference_s,
            rtf
        );
    }
    println!(
        "  CER(raw)={:.2}% CER(norm)={:.2}%",
        summary.cer_raw * 100.0,
        summary.cer_norm * 100.0
    );
    println!(
        "  hyp_chars={} ref_chars={}",
        summary.hyp_chars, summary.ref_chars
    );
    print_alignment(&align);

    // 清理抽帧目录（对齐 runBenchmarkCommon 的 rm -rf frameDir）
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// 与 `run_benchmark_cpp` 完全对称，仅把 OCR 调用换成 rust 实现
/// （`ocr_dir_rust` / `ocr_frame_rust`），引擎标签改为 `ort-rust`。
/// 帧抽取、合并、CER、时序对齐、summary 复用同一套代码，保证两实现可横比。
fn run_benchmark_rust(
    label: &str,
    fps: f64,
    text_score: f64,
    subtitle_only: bool,
    use_dir: bool,
    threads: Option<usize>,
) {
    let vpath = video_path();
    let out_dir = tmp_dir().join(format!("frames-{}", label));
    std::fs::create_dir_all(&out_dir).unwrap();

    println!("\n=== OCR Benchmark: {} (fps={}, engine=ort-rust, mode={}, threads={}) ===",
        label, fps, if use_dir { "--dir" } else { "single-frame" },
        threads.map(|n| n.to_string()).unwrap_or_else(|| "default".into()));
    println!("  extracting frames...");
    let (duration_s, step, src_fps) = extract_frames(&vpath, &out_dir, fps);
    let frame_files = list_frame_files(&out_dir);

    let mut frame_results: Vec<FrameResult> = Vec::new();
    let mut total_ms = 0.0f64;
    let mut total_det = 0.0f64;
    let mut total_post = 0.0f64;
    let mut total_rec = 0.0f64;

    if use_dir {
        let results = ocr_dir_rust(&out_dir, Some(text_score), subtitle_only, threads);
        for (i, r) in results.into_iter().enumerate() {
            let timestamp = (((i as f64) * (step as f64)) / src_fps * 1000.0).round() as u64;
            frame_results.push(FrameResult {
                text: r.text.clone(),
                timestamp,
                confidence: r.confidence,
                bbox: None,
            });
            total_ms += r.total_ms;
            total_det += r.det_ms;
            total_post += r.post_ms;
            total_rec += r.rec_ms;
        }
    } else {
        for (i, f) in frame_files.iter().enumerate() {
            let r = ocr_frame_rust(f, Some(text_score), subtitle_only, threads);
            let timestamp = (((i as f64) * (step as f64)) / src_fps * 1000.0).round() as u64;
            frame_results.push(FrameResult {
                text: r.text.clone(),
                timestamp,
                confidence: r.confidence,
                bbox: None,
            });
            total_ms += r.total_ms;
            total_post += r.post_ms;
            total_rec += r.rec_ms;
            total_det += r.det_ms;
        }
    }

    let (merged_text, segments) = merge_frames(&frame_results);
    let inference_s = total_ms / 1000.0;
    let rtf = if duration_s > 0.0 {
        inference_s / duration_s
    } else {
        0.0
    };
    let has_timings = total_det > 0.0 || total_post > 0.0 || total_rec > 0.0;

    let ocr_output = serde_json::json!({
        "audio_info": {
            "duration": if segments.is_empty() { (duration_s * 1000.0).round() as u64 }
                        else { segments.last().unwrap().end }
        },
        "result": {
            "text": merged_text,
            "segments": segments.iter().map(|s| serde_json::json!({
                "text": s.text,
                "start": s.start,
                "end": s.end,
                "confidence": s.confidence,
            })).collect::<Vec<_>>(),
        },
        "_engine": "ort-rust",
        "_source": "video_hardsub",
        "_fps": fps,
        "_textScore": text_score,
        "_subtitleOnly": subtitle_only,
        "_timingsMs": if has_timings {
            serde_json::json!({
                "total": total_ms.round() as u64,
                "averagePerFrame": (total_ms / frame_results.len().max(1) as f64).round() as u64,
                "det": total_det.round() as u64,
                "post": total_post.round() as u64,
                "rec": total_rec.round() as u64,
            })
        } else {
            serde_json::Value::Null
        },
    });

    let metadata_dir = tmp_dir().join(label).join("metadata");
    std::fs::create_dir_all(&metadata_dir).unwrap();
    let ocr_path = metadata_dir.join("ocr.json");
    std::fs::write(&ocr_path, serde_json::to_string_pretty(&ocr_output).unwrap()).unwrap();

    let cer = eval_cer(&ocr_path, &gt_path());
    let align = align_segments(
        &load_timed_segments(&gt_path()),
        &load_timed_segments(&ocr_path),
    );

    let summary = Summary {
        label: label.to_string(),
        fps,
        engine: "ort-rust".to_string(),
        frames: frame_results.len(),
        segments: segments.len(),
        audio_duration_s: (duration_s * 10.0).round() / 10.0,
        ocr_inference_s: (inference_s * 1000.0).round() / 1000.0,
        ocr_rtf: (rtf * 10000.0).round() / 10000.0,
        cer_raw: cer.raw,
        cer_norm: cer.norm,
        hyp_chars: cer.hyp_chars,
        ref_chars: cer.ref_chars,
        text_score,
        subtitle_only,
        use_dir,
        threads,
        timings: if has_timings {
            Some(Timings {
                total: total_ms,
                avg_per_frame: total_ms / frame_results.len().max(1) as f64,
                det: total_det,
                post: total_post,
                rec: total_rec,
            })
        } else {
            None
        },
    };

    let summary_json = serde_json::json!({
        "label": summary.label,
        "fps": summary.fps,
        "engine": summary.engine,
        "frames": summary.frames,
        "segments": summary.segments,
        "audio_duration_s": summary.audio_duration_s,
        "ocr_inference_s": summary.ocr_inference_s,
        "ocr_rtf": summary.ocr_rtf,
        "wer": summary.cer_norm,
        "cer": summary.cer_norm,
        "hyp_chars": summary.hyp_chars,
        "ref_chars": summary.ref_chars,
        "textScore": summary.text_score,
        "subtitleOnly": summary.subtitle_only,
        "mode": if summary.use_dir { "dir" } else { "single-frame" },
        "intraOpThreads": summary.threads,
        "alignment": {
            "paired": align.pairs.len(),
            "missed": align.missed,
            "spurious": align.spurious,
            "zero_duration": align.zero_duration,
            "split": align.split,
            "merged": align.merged,
            "iou_mean": (align.iou_mean * 10000.0).round() / 10000.0,
            "paired_cer": align.paired_cer,
            "start_delta_ms": {
                "mean": align.start_delta_mean.round(),
                "median": align.start_delta_median.round(),
                "p95_abs": align.start_delta_p95_abs.round(),
            },
            "end_delta_ms": {
                "mean": align.end_delta_mean.round(),
                "median": align.end_delta_median.round(),
                "p95_abs": align.end_delta_p95_abs.round(),
            },
        },
        "ocr_timings_ms": summary.timings.as_ref().map(|t| serde_json::json!({
            "total": t.total.round() as u64,
            "avgPerFrame": t.avg_per_frame.round() as u64,
            "det": t.det.round() as u64,
            "post": t.post.round() as u64,
            "rec": t.rec.round() as u64,
        })),
    });
    std::fs::write(
        metadata_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary_json).unwrap(),
    )
    .unwrap();

    println!(
        "  merged {} frames → {} segments",
        frame_results.len(),
        segments.len()
    );
    println!("  segs={} dur={:.1}s", segments.len(), duration_s);
    if has_timings {
        println!(
            "  det={}ms post={}ms rec={}ms total={}ms",
            total_det.round() as u64,
            total_post.round() as u64,
            total_rec.round() as u64,
            total_ms.round() as u64
        );
        println!(
            "  avg/frame={}ms  inference={:.3}s RTF={:.4}",
            (total_ms / frame_results.len().max(1) as f64).round() as u64,
            inference_s,
            rtf
        );
    }
    println!(
        "  CER(raw)={:.2}% CER(norm)={:.2}%",
        summary.cer_raw * 100.0,
        summary.cer_norm * 100.0
    );
    println!(
        "  hyp_chars={} ref_chars={}",
        summary.hyp_chars, summary.ref_chars
    );
    print_alignment(&align);

    let _ = std::fs::remove_dir_all(&out_dir);
}

/// 对齐 eval-ocr.ts main()：读 hyp/GT 的 result.segments，join 文本，算 raw + norm CER。
struct CerResult {
    raw: f64,
    norm: f64,
    hyp_chars: usize,
    ref_chars: usize,
}

fn load_segments_text(path: &Path) -> Vec<String> {
    let content = std::fs::read_to_string(path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    v.get("result")
        .and_then(|r| r.get("segments"))
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                .map(|t| t.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 打印时序对齐质量。delta 正值 = hyp 偏晚，负值 = 偏早。
fn print_alignment(a: &AlignReport) {
    println!(
        "  aligned: paired={} missed={} spurious={} split={} merged={} zero-dur={}",
        a.pairs.len(),
        a.missed,
        a.spurious,
        a.split,
        a.merged,
        a.zero_duration
    );
    println!(
        "  IoU(mean)={:.4}  CER(paired)={:.2}%",
        a.iou_mean,
        a.paired_cer * 100.0
    );
    println!(
        "  start Δms: mean={:+.0} median={:+.0} p95|Δ|={:.0}",
        a.start_delta_mean, a.start_delta_median, a.start_delta_p95_abs
    );
    println!(
        "  end   Δms: mean={:+.0} median={:+.0} p95|Δ|={:.0}",
        a.end_delta_mean, a.end_delta_median, a.end_delta_p95_abs
    );
}

/// 读带时间戳的 segments，用于时序对齐评估。
fn load_timed_segments(path: &Path) -> Vec<TimedText> {
    let content = std::fs::read_to_string(path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    v.get("result")
        .and_then(|r| r.get("segments"))
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(TimedText {
                        text: s.get("text").and_then(|t| t.as_str())?.trim().to_string(),
                        start: s.get("start").and_then(|t| t.as_u64())?,
                        end: s.get("end").and_then(|t| t.as_u64())?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn eval_cer(hyp_path: &Path, gt_path: &Path) -> CerResult {
    let gt_segs = load_segments_text(gt_path);
    let hyp_segs = load_segments_text(hyp_path);
    let gt_full: String = gt_segs.concat();
    let hyp_full: String = hyp_segs.concat();

    let raw = compute_cer(&strip_ws(&gt_full), &strip_ws(&hyp_full));
    let norm = compute_cer(&normalize_for_cer(&gt_full), &normalize_for_cer(&hyp_full));

    CerResult {
        raw,
        norm,
        hyp_chars: strip_ws(&hyp_full).chars().count(),
        ref_chars: strip_ws(&gt_full).chars().count(),
    }
}

fn strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut impl_ = "cpp".to_string();
    let mut fps = 2.0f64;
    let mut text_score = 0.45f64;
    let mut subtitle_only = true;
    let mut only: Option<String> = None;
    let mut use_dir = false;
    // None = 不传 --threads，由 cpp 侧默认值决定
    let mut threads: Option<usize> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--impl" => {
                if let Some(v) = args.get(i + 1) {
                    impl_ = v.clone();
                }
                i += 2;
            }
            "--fps" => {
                if let Some(v) = args.get(i + 1) {
                    fps = v.parse().unwrap_or(2.0);
                }
                i += 2;
            }
            "--text-score" => {
                if let Some(v) = args.get(i + 1) {
                    text_score = v.parse().unwrap_or(0.45);
                }
                i += 2;
            }
            "--subtitle-only" => {
                subtitle_only = true;
                i += 1;
            }
            "--threads" => {
                if let Some(v) = args.get(i + 1) {
                    threads = v.parse().ok().filter(|n| *n > 0);
                }
                i += 2;
            }
            "--dir" => {
                use_dir = true;
                i += 1;
            }
            "--only" => {
                if let Some(v) = args.get(i + 1) {
                    only = Some(v.clone());
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    // 对齐 label 生成规则：ocr-${engine}-fps${fps}${so?-so}${-ts${textScore}}
    let engine = match impl_.as_str() {
        "cpp" => "cpp",
        "py" => "python",
        "rust" => "rust",
        other => other,
    };
    let ts_label = format!("-ts{}", text_score);
    let base_label = format!(
        "ocr-{}-fps{}{}{}",
        engine,
        fps,
        if subtitle_only { "-so" } else { "" },
        ts_label
    );

    let label = match &only {
        Some(o) => o.clone(),
        None => base_label,
    };

    match impl_.as_str() {
        "cpp" => run_benchmark_cpp(&label, fps, text_score, subtitle_only, use_dir, threads),
        "py" => {
            eprintln!("[py] 基准尚未实现（未装 rapidocr_onnxruntime）");
            std::process::exit(1);
        }
        "rust" => run_benchmark_rust(&label, fps, text_score, subtitle_only, use_dir, threads),
        other => {
            eprintln!("未知实现: {}", other);
            std::process::exit(2);
        }
    }
}
