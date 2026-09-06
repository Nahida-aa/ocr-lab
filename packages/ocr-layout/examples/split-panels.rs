//! 命令行：把多格漫画长图切成紧致的独立子图。
//!
//!   cargo run -p ocr-layout --example split-panels -- <图片或目录> [--out <dir>] [--threshold N] [--gradient-threshold N] [--max-trim N] [--min-gap N] [--mode sprite|free|auto]
//!
//! 把「纯图片构成的 UI」交给布局层：每一格就是一条**无文字信号的 Widget**
//! （source=Color）。算法分两层：
//!
//! 1. **行投影切行**：`row_dark[y]` = 该行最暗像素亮度；整行无内容的连续行
//!    （亮度 >= `threshold`，需容忍 JPEG 脏白）达到 `min_gap` 视为格间分隔带，
//!    带间即行块。不做网格整齐排列假设。
//!
//! 2. **四边紧致化（直线突变检测）**：切出的行块边缘常有振铃/浅灰渐变白边
//!    （min 198–253 不等，固定阈值裁不净）。边界判据是**沿直线方向的突变**：
//!    一条垂直边界意味着「同一 x 上，行块内连续多个 y 都有 x 与 x+1 间的色差
//!    突变」；水平边界同理按 y 聚合。**突变的 x 不变或 y 不变**——斜线（如画面
//!    里的桌沿）在每个 y 上的突变发生在不同 x，占比极低，不会被误判为边界
//!    （这是行最暗值曲线找不到干净跳变的根本原因：斜线把行信号拖了尾）。
//!
//!    另有浅色带先验：候选边界的外侧（边缘侧）必须是浅色带（行/列 min 的均值
//!    >= 190），画面直接顶到边缘（深色内容）时不裁，防止画面内部的水平/垂直
//!    直线被误当边界；`max-trim` 限制扫描深度兜底。
//!
//! 输出：总是同时产出 `<name>-preview.png`（带彩色框线的原图，annotate 复用，
//! 供人工核对）与紧致裁剪的 `<name>-<序号>.png`，写入 `<输入所在目录>/out/`。

use anyhow::Context;
use image::RgbImage;
use ocr_layout::{Widget, WidgetSource, annotate};
use std::path::{Path, PathBuf};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        bail_usage()?;
    }

    // 手动解析（对齐 examples/layout.rs 的风格）。
    let mut input: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut threshold: u8 = 195;
    let mut gradient_threshold: i32 = 40;
    let mut max_trim: usize = 60;
    let mut min_gap: u32 = 6;
    // sprite: 组内统一到最紧共同边界 (卡片等大, 零白边);
    // free: 逐格独立紧致 (布局各异的自由拼图, 不等大);
    // auto: 按组内边界极差自动判定 (极差 <= 12px 视为等大卡片)。
    let mut mode = String::from("auto");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                out_dir = Some(PathBuf::from(args.get(i + 1).context("--out 缺少参数")?));
                i += 2;
            }
            "--threshold" => {
                threshold = args.get(i + 1).context("--threshold 缺少参数")?.parse()?;
                i += 2;
            }
            "--gradient-threshold" => {
                gradient_threshold = args
                    .get(i + 1)
                    .context("--gradient-threshold 缺少参数")?
                    .parse()?;
                i += 2;
            }
            "--max-trim" => {
                max_trim = args.get(i + 1).context("--max-trim 缺少参数")?.parse()?;
                i += 2;
            }
            "--min-gap" => {
                min_gap = args.get(i + 1).context("--min-gap 缺少参数")?.parse()?;
                i += 2;
            }
            "--mode" => {
                mode = args.get(i + 1).context("--mode 缺少参数")?.to_string();
                i += 2;
            }
            other if other.starts_with('-') => {
                anyhow::bail!("未知参数: {other}");
            }
            other => {
                input = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }
    if !matches!(mode.as_str(), "sprite" | "free" | "auto") {
        anyhow::bail!("--mode 仅支持 sprite | free | auto");
    }
    let input = input.context("缺少输入（图片文件或目录）")?;

    let inputs = collect_images(&input)?;
    if inputs.is_empty() {
        anyhow::bail!("{} 下没有找到图片", input.display());
    }

    let out_dir = out_dir.unwrap_or_else(|| {
        let base = if input.is_dir() {
            input.clone()
        } else {
            input.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        base.join("out")
    });
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;

    // 先对每张图跑切分，收集 (源名, 源宽, 源高, rects)。
    let mut results: Vec<(String, u32, u32, Vec<(u32, u32, u32, u32)>)> = Vec::new();
    for src in &inputs {
        let name = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "img".into());
        let img = image::open(src)
            .with_context(|| format!("读取图片失败: {}", src.display()))?
            .to_rgb8();

        let rects = split_panels(&img, threshold, gradient_threshold, max_trim, min_gap);
        eprintln!(
            "[split] {} {}x{} → {} 块",
            src.display(),
            img.width(),
            img.height(),
            rects.len()
        );
        results.push((name, img.width(), img.height(), rects));
    }

    // 组内一致性：同一部漫画的卡片理论上等大；梯度紧致在渐变白边上的跳变
    // 位置有 ±几 px 噪声，导致同源格子尺寸漂移（实测 872/868/865 渐小）。
    let mode = match mode.as_str() {
        "sprite" => Mode::Sprite,
        "free" => Mode::Free,
        _ => Mode::Auto,
    };
    unify_sizes(&mut results, mode);

    // 输出：预览总是产出 + 紧致裁剪。
    let mut total = 0usize;
    for (name, w, h, rects) in &results {
        let img = image::open(
            inputs
                .iter()
                .find(|s| s.file_stem().map(|s| s.to_string_lossy().into_owned()).as_deref() == Some(name.as_str()))
                .context("源图丢失")?,
        )
        .with_context(|| format!("读取图片失败: {name}"))?
        .to_rgb8();

        let widgets: Vec<Widget> = rects
            .iter()
            .enumerate()
            .map(|(idx, r)| Widget {
                id: idx,
                label: String::new(),
                rect: *r,
                color: [255, 0, 0],
                area_ratio: (r.2 * r.3) as f32 / (*w * *h) as f32,
                source: WidgetSource::Color,
            })
            .collect();
        let preview = out_dir.join(format!("{name}-preview.png"));
        annotate(&img, &widgets)
            .save(&preview)
            .with_context(|| format!("保存失败: {}", preview.display()))?;
        eprintln!("        预览 → {}", preview.display());

        for (idx, (x, y, cw, ch)) in rects.iter().enumerate() {
            let out = out_dir.join(format!("{name}-{}.png", idx + 1));
            crop_rgb(&img, *x, *y, *cw, *ch)
                .save(&out)
                .with_context(|| format!("保存失败: {}", out.display()))?;
            eprintln!("        {}-{}  rect=({x},{y},{cw},{ch})", name, idx + 1);
            total += 1;
        }
    }
    eprintln!("[done] 共 {total} 块 → {}", out_dir.display());
    Ok(())
}

/// 拼接布局模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 精灵图：卡片等大，组内统一到最紧共同边界（零白边）。
    Sprite,
    /// 自由拼图：逐格独立紧致，不等大。
    Free,
    /// 自动：按组内边界极差判定（≤ 12px 视为等大卡片）。
    Auto,
}

/// 自动判定的边界极差上限（px）：组内参与格的 left / x1 / 高度极差都不超过
/// 此值时视为「等大卡片」（精灵图），否则视为自由布局。
const SPRITE_SPREAD_MAX: u32 = 12;

/// 组内尺寸一致性（精灵图模式）。
///
/// 梯度紧致在渐变白边上的跳变位置有 ±几 px 噪声，同源格子会漂移出
/// 872/868/865 这种渐小尺寸。统一到组内**最紧共同边界**：
/// `left = min`、`x1 = max`、`height = max`——每格都裁到组内最紧，
/// 零白边保证；多裁的部分是边缘渐变带（1-4px，无视觉内容）。
///
/// 参与格 = 边界可信的格子：`x0 > 0` 且 `x1 < w` 且 `y1 < h`。
/// 贴源图边的格子（如 4.jpg 底格卡片超出源图、全宽到底）边界不可信，
/// 不参与统计也保持原样——统一它们会裁掉真实内容。
///
/// 自由模式（Free）跳过统一：布局各异的拼图逐格最优、不等大。
/// 自动模式（Auto）先算组内极差再决定。
fn unify_sizes(
    results: &mut [(String, u32, u32, Vec<(u32, u32, u32, u32)>)],
    mode: Mode,
) {
    use std::collections::HashMap;

    let mut groups: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, (_, w, _, _)) in results.iter().enumerate() {
        groups.entry(*w).or_default().push(i);
    }

    for (_, idxs) in groups {
        // 组内参与格（边界可信）与它们的原始边界。
        let mut lefts: Vec<u32> = Vec::new();
        let mut x1s: Vec<u32> = Vec::new();
        let mut heights: Vec<u32> = Vec::new();
        for &i in &idxs {
            let (_, w, h, rects) = &results[i];
            for r in rects {
                let (x0, y0, rw, rh) = *r;
                if x0 == 0 || x0 + rw == *w || y0 + rh == *h {
                    continue; // 贴边格：卡片被源图裁断，边界不可信
                }
                lefts.push(x0);
                x1s.push(x0 + rw);
                heights.push(rh);
            }
        }
        if lefts.len() < 2 {
            continue; // 可信格不足 2 个，无一致性可言
        }

        // Auto 判据：三个维度的极差都在限内 → 等大卡片（精灵图）。
        if mode == Mode::Auto {
            let spread = |v: &[u32]| v.iter().max().unwrap() - v.iter().min().unwrap();
            let is_sprite = spread(&lefts) <= SPRITE_SPREAD_MAX
                && spread(&x1s) <= SPRITE_SPREAD_MAX
                && spread(&heights) <= SPRITE_SPREAD_MAX;
            if !is_sprite {
                eprintln!("[mode] auto → free (边界极差超 {}px)", SPRITE_SPREAD_MAX);
                continue;
            }
        }
        if mode == Mode::Free {
            continue;
        }

        // Sprite：统一到最紧共同边界（min/max）——零白边保证。
        let ul = *lefts.iter().min().unwrap();
        let ux1 = *x1s.iter().max().unwrap();
        let uh = *heights.iter().max().unwrap();
        for &i in &idxs {
            let (_, w, h, rects) = &mut results[i];
            for r in rects.iter_mut() {
                if r.0 == 0 || r.0 + r.2 == *w || r.1 + r.3 == *h {
                    continue; // 特殊格（贴源图边）保持原样
                }
                *r = (ul, r.1, ux1 - ul, uh);
            }
        }
    }
}


fn bail_usage() -> anyhow::Result<()> {
    anyhow::bail!(
        "用法: split-panels <图片或目录> [--out <dir>] [--threshold N] [--gradient-threshold N] [--max-trim N] [--min-gap N] [--mode sprite|free|auto]"
    )
}

/// 收集要处理的图片（单文件或目录下的常见格式）。
fn collect_images(input: &Path) -> anyhow::Result<Vec<PathBuf>> {
    const EXTS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "bmp"];
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(input)
        .with_context(|| format!("读取目录失败: {}", input.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    out.sort();
    Ok(out)
}

/// 核心：行投影切行 + 四边紧致化（直线突变检测）。见模块文档。
fn split_panels(
    img: &RgbImage,
    threshold: u8,
    gradient_threshold: i32,
    max_trim: usize,
    min_gap: u32,
) -> Vec<(u32, u32, u32, u32)> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    // 每像素最暗通道值：分隔带检测与突变统计的聚合信号。
    let darkness: Vec<u8> = img
        .pixels()
        .map(|p| *p.0.iter().min().unwrap())
        .collect();

    // 行投影 → 行块。max 滤波只喂给分隔带检测（填平噪声暗点）；
    // 突变统计与浅色带判定用原始曲线——滤波会抹平内容/白边交界的内容侧,
    // 消灭紧致化依赖的跳变（实测 2-1 右边 11 列白边因此回漏）。
    let row_dark_raw: Vec<u8> = (0..h)
        .map(|y| (0..w).map(|x| darkness[y * w + x]).min().unwrap())
        .collect();
    let row_dark: Vec<u8> = max_filter3(&row_dark_raw);

    let min_area = (0.005f64 * (w * h) as f64).max(1.0) as usize;
    let mut rects = Vec::new();
    for (y0, y1) in bright_blocks(&row_dark, threshold, min_gap) {
        // 行块边界已由 195 分隔带给出（白边整段并入分隔带），顶/底无需紧致。
        //
        // 左/右紧致：col_dark（列最暗值）曲线上，从边缘向内找第一个显著跳变。
        // 不能用「逐像素色差占比」——白边到内容是跨几列的渐变（253→219→180→144），
        // 同一 y 上相邻列差不足阈值，占比永远不达标（实测 2-1 右侧 11 列漏裁）；
        // 而 col_min 曲线把每列压缩成一个统计值，跳变（237→180=57）清晰可测。
        let col_dark: Vec<u8> = (0..w)
            .map(|x| (y0..y1).map(|y| darkness[y * w + x]).min().unwrap())
            .collect();
        // col_dark 是全宽序列：左边界从 x=0 向内、右边界从 x=w-1 向内找跳变。
        let left = edge_gradient_trim(&col_dark, gradient_threshold, max_trim, true);
        let right = edge_gradient_trim(&col_dark, gradient_threshold, max_trim, false);
        let x0t = left;
        let x1t = w - right;
        if x1t <= x0t {
            continue;
        }

        let (bw, bh) = (x1t - x0t, y1 - y0);
        // 面积下限：滤噪点（相对全图 0.5%）。
        if bw * bh < min_area {
            continue;
        }
        rects.push((x0t as u32, y0 as u32, bw as u32, bh as u32));
    }
    rects
}

/// 从曲线一端向内找第一个显著跳变，返回应裁掉的像素数（0 = 不裁）。
///
/// 白边的特征是「从边缘开始一段平坦的浅色，然后跳变到内容」；画面直接顶到
/// 边缘时边缘像素本身是深色内容，此时不裁。`max_trim` 限制扫描深度——跳变
/// 必须发生在边缘窄条内，画面内部的大反差不会被误当边界。
fn edge_gradient_trim(dark: &[u8], gradient_threshold: i32, max_trim: usize, from_start: bool) -> usize {
    let seq: Vec<i32> = if from_start {
        dark.iter().map(|&v| v as i32).collect()
    } else {
        dark.iter().rev().map(|&v| v as i32).collect()
    };
    if seq.is_empty() {
        return 0;
    }
    // 边缘像素已是深色内容（画面顶到边）→ 不裁。
    if seq[0] < 150 {
        return 0;
    }
    let limit = max_trim.min(seq.len().saturating_sub(1));
    for x in 0..limit {
        if (seq[x + 1] - seq[x]).abs() >= gradient_threshold {
            return x + 1;
        }
    }
    0
}

/// 窗口 3 的 max 滤波：填平投影曲线上孤立的暗点。
///
/// 白边/分隔带内部的 JPEG 噪声暗列会把连续亮段切碎（亮段 < min_gap 被丢弃，
/// 白列就漏进了内容块）。max 滤波让宽度 1 的暗点被邻居抬高，亮段恢复连续。
fn max_filter3(dark: &[u8]) -> Vec<u8> {
    let n = dark.len();
    (0..n)
        .map(|i| {
            let a = dark[i.saturating_sub(1)];
            let b = dark[i];
            let c = dark[(i + 1).min(n - 1)];
            a.max(b).max(c)
        })
        .collect()
}

/// 从最暗值序列找内容区间：连续亮（>= 阈值）且长度 >= `min_gap` 的段为分隔，
/// 分隔之间的部分即内容块。
fn bright_blocks(dark: &[u8], threshold: u8, min_gap: u32) -> Vec<(usize, usize)> {
    let min_gap = min_gap as usize;
    let mut gaps: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &v) in dark.iter().enumerate() {
        if v >= threshold {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            if i - s >= min_gap {
                gaps.push((s, i));
            }
        }
    }
    if let Some(s) = start
        && dark.len() - s >= min_gap
    {
        gaps.push((s, dark.len()));
    }

    let mut blocks = Vec::new();
    let mut prev = 0usize;
    for (a, b) in gaps {
        if a > prev {
            blocks.push((prev, a));
        }
        prev = b;
    }
    if dark.len() > prev {
        blocks.push((prev, dark.len()));
    }
    blocks
}

/// 从源图裁出矩形（手写像素拷贝，不依赖 image 的 crop API 版本差异）。
fn crop_rgb(img: &RgbImage, x: u32, y: u32, w: u32, h: u32) -> RgbImage {
    let mut out = RgbImage::new(w, h);
    for yy in 0..h {
        for xx in 0..w {
            *out.get_pixel_mut(xx, yy) = *img.get_pixel(x + xx, y + yy);
        }
    }
    out
}
