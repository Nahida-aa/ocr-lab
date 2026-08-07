//! 命令行：`subtitle-ocr <image> [text_score]` 或 `subtitle-ocr --dir <dir> ...`
//!
//! 纯感知 OCR 工具，对标 cpp 的 `ocr_pipeline.cpp`：输出 JSON 数组，
//! 每个元素含 `text` / `confidence` / `boxes` / `timestamp_ms`。
//!
//! 不含任何耗时字段——推理耗时是旁路观测数据，由调用方自行计时（CLI 在
//! `ocr_image` 调用前后 `Instant::now()` 测量，打到 stderr 的 `[ocr] ... det=...ms`；
//! benchmark 同理）。不污染 stdout 的 JSON 数组。
//!
//! 本 CLI 只做「逐图/批量 OCR」，不输出时间轴、不做帧合并——带时间戳的字幕段
//! 由知道视频结构的上游（自行补 `start`/`end` 后调用 `subtitle-ocr-merge`）负责。

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use ndarray::Array3;
use serde_json::Value;
use std::path::{Path, PathBuf};
use subtitle_ocr::{OcrOptions, SubtitleOcr};

/// 批量模式下，文件名不符合时间格式时的处理策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum BadNameAction {
    /// 跳过该文件并打警告（默认）。
    Skip,
    /// 直接报错终止。
    Error,
}

/// 目录里一张待识别图片的解析结果。
///
/// `times` 为该图对应的时刻列表：
/// - `ms` 文件名 → 单元素 `[t]`
/// - `ms_ms` 文件名 → 双元素 `[start, end]`（同一张图仅识别一次，产出两个 FrameResult）
struct DirEntry {
    path: PathBuf,
    times: Vec<u64>,
}

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
    /// `skip` 跳过并警告（默认）；`error` 直接报错终止。
    #[arg(long, value_enum, default_value_t = BadNameAction::Skip)]
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

/// 解析文件名里的时刻：支持 `ms` 或 `ms_ms`，可前置多余 0。
///
/// 例如 `001234.png` → `[1234]`；`001234_001250.png` → `[1234, 1250]`。
/// 返回 `None` 表示文件名不符合格式（无扩展名 / 非纯数字 / 段数 >2）。
fn parse_name_times(stem: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    let mut times = Vec::with_capacity(parts.len());
    for p in parts {
        // 允许前置多余 0；空段（如 `__`）非法。
        if p.is_empty() {
            return None;
        }
        // 仅接受十进制数字（前置 0 自动被 u64 解析忽略）。
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        times.push(p.parse::<u64>().ok()?);
    }
    Some(times)
}

/// 列出目录下图片文件，解析文件名时刻并按数值时间排序（对齐 cpp listFrames）。
///
/// 文件名须为 `ms` 或 `ms_ms`（可前置 0）形式，否则按 `--on-bad-name` 处理：
/// `skip` 跳过并警告，`error` 直接报错。返回的每个 [`DirEntry`] 带解析出的时刻列表。
fn list_frames(dir: &Path, on_bad: BadNameAction) -> Result<Vec<DirEntry>> {
    let mut entries: Vec<DirEntry> = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for e in read.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase());
            if !matches!(ext.as_deref(), Some("jpg" | "jpeg" | "png" | "bmp")) {
                continue;
            }
            let stem = p
                .file_stem()
                .and_then(|s| s.to_str())
                .context("文件名非 UTF-8")?;
            match parse_name_times(stem) {
                Some(times) => entries.push(DirEntry { path: p, times }),
                None => match on_bad {
                    BadNameAction::Skip => {
                        eprintln!("跳过（文件名不符合 ms/ms_ms 格式）: {}", p.display());
                    }
                    BadNameAction::Error => {
                        anyhow::bail!("文件名不符合 ms/ms_ms 时间格式: {}", p.display());
                    }
                },
            }
        }
    }
    // 按首个时刻数值排序（保证时间顺序，不受前置 0 / 字典序影响）。
    entries.sort_by_key(|e| e.times.first().copied().unwrap_or(0));
    Ok(entries)
}

/// 单帧输出直接复用库里的 [`subtitle_ocr::FrameResult`]（文本 / 聚合置信度 /
/// 各框明细 / 几何值域 / 对应时刻），不再另定义输出结构。
///
/// 不含输入文件名——调用方本就知道自己喂了哪张图，批量模式下时刻信息已体现在
/// `timestamp_ms`；也不含耗时字段——推理耗时是旁路观测数据，由调用方自行计时
/// （在 `ocr_image` 调用前后 `Instant::now()` 测量），不污染 JSON 结构。
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

    // 构建待识别条目：--dir 一张图可对应 1~2 个时刻（ms_ms 双时刻），
    // 单图 <image> 对应 1 个时刻（0 = 无时间）。
    let entries: Vec<DirEntry> = if let Some(dir) = &cli.dir {
        let dir = resolve_path(&repo_root, dir);
        list_frames(&dir, cli.on_bad_name)?
    } else if let Some(img) = &cli.image {
        vec![DirEntry {
            path: resolve_path(&repo_root, img),
            times: vec![0],
        }]
    } else {
        anyhow::bail!("必须提供 <image> 或 --dir <dir>");
    };

    let mut frame_outs: Vec<subtitle_ocr::FrameResult> = Vec::with_capacity(entries.len());

    for entry in entries.iter() {
        // 仅识别一次：ms_ms 的同一张图读图 + OCR 一次。
        let rgb = load_rgb(&entry.path)?;
        let boxes = ocr.ocr_image(&rgb)?;
        // 聚合一次（text 拼接 / confidence 取均值 / 值域），timestamp 先置 0。
        let aggregated = subtitle_ocr::aggregate_boxes(&boxes);

        // 按文件名时刻展开：ms_ms 同一张图产生多个 FrameResult，仅覆写 timestamp_ms，
        // 不再重复聚合。
        for &ts in &entry.times {
            let mut fr = aggregated.clone();
            fr.timestamp_ms = ts;
            frame_outs.push(fr);
        }
    }

    // 主输出：与 cpp 同形状的 JSON 数组（逐图/批量，不带时间轴）。
    let arr: Vec<Value> = frame_outs
        .iter()
        .map(|f| serde_json::to_value(f).unwrap_or(Value::Null))
        .collect();

    println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_name_times_single() {
        assert_eq!(parse_name_times("001234"), Some(vec![1234]));
        assert_eq!(parse_name_times("0"), Some(vec![0]));
        assert_eq!(parse_name_times("999"), Some(vec![999]));
    }

    #[test]
    fn parse_name_times_range() {
        // ms_ms：两段，前置 0 不影响数值。
        assert_eq!(parse_name_times("001234_001250"), Some(vec![1234, 1250]));
        assert_eq!(parse_name_times("1234_1250"), Some(vec![1234, 1250]));
    }

    #[test]
    fn parse_name_times_invalid() {
        // 三段（超过 2 段）非法。
        assert_eq!(parse_name_times("132_932_0"), None);
        // 非数字 / 空段非法。
        assert_eq!(parse_name_times("abc"), None);
        assert_eq!(parse_name_times("1234_"), None);
        assert_eq!(parse_name_times("_1234"), None);
        // 带扩展名前缀（整名）不在此函数处理范围，但这里只测 stem。
        assert_eq!(parse_name_times("12.3"), None);
    }

    /// 在临时目录放若干图片，验证 list_frames 的解析/排序/双产出/skip。
    fn make_tmp_dir(files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sf_ocr_list_{}_{}",
            std::process::id(),
            files.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"dummy").unwrap();
        }
        dir
    }

    #[test]
    fn list_frames_parses_and_sorts() {
        let dir = make_tmp_dir(&[
            "00500.png",
            "00100.png",
            "00300_00350.png", // ms_ms：双时刻
            "ignore.txt",      // 非图片，跳过
        ]);
        let entries = list_frames(&dir, BadNameAction::Skip).unwrap();
        // 按首个时刻排序：100, 300(eff), 500。
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].times, vec![100]);
        assert_eq!(entries[1].times, vec![300, 350]); // 双产出
        assert_eq!(entries[2].times, vec![500]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_frames_bad_name_error() {
        let dir = make_tmp_dir(&["00100_00150.png", "badname.png"]);
        let r = list_frames(&dir, BadNameAction::Error);
        assert!(r.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_frames_bad_name_skip() {
        let dir = make_tmp_dir(&["00100_00150.png", "badname.png"]);
        let entries = list_frames(&dir, BadNameAction::Skip).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].times, vec![100, 150]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
