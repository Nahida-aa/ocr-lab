//! 命令行：把多格漫画长图切成紧致的独立子图。
//!
//!   cargo run -p ocr-layout --example split-panels -- <图片或目录> [--out <dir>] [--threshold N] [--min-gap N] [--mode sprite|free|auto]
//!
//! 把「纯图片构成的 UI」交给布局层：每一格就是一条**无文字信号的 Widget**
//! （source=Color）。算法分三层：
//!
//! 1. **行投影切行**：`row_dark[y]` = 该行最暗像素亮度；整行无内容的连续行
//!    （亮度 >= `threshold`，需容忍 JPEG 脏白）达到 `min_gap` 视为格间分隔带，
//!    带间即行块。不做网格整齐排列假设。
//!
//! 2. **四边收缩**：行块边缘常有振铃/浅灰渐变白边（min 198–253 不等）。从每
//!    条边向内收缩「整行/列 min >= threshold」的浅色边缘——阈值 195 恰好分离
//!    两类区域：白边/画布 (min >= 195) 与卡片内容 (min <= 180，含灰蓝背景)。
//!    渐变过渡不需要「突变」：只要内容首行/列比白边深，收缩就会停在那里。
//!
//!    历史教训（都已实测）：梯度跳变检测在渐变边界上时灵时不灵（2-1 右侧
//!    11 列漏裁）；逐像素占比判据同因（渐变中相邻像素差不足）；顶/底紧致会
//!    被画面内斜线（桌沿）拖尾误裁 3-2 底部 37px——行块边界由分隔带给出后，
//!    这些花活全部不需要。
//!
//! 3. **模式**（`--mode`）：
//!    - `sprite`（精灵图）：同源卡片等大。各格收缩后的尺寸有 ±几 px 噪声
//!      （渐变带内边缘位置的抖动），统一到组内**最紧**（left=max、x1=min、
//!      height=min）——零白边保证；多裁的 1-4px 是边缘渐变带，无视觉内容。
//!    - `free`（自由拼图）：逐格保留自己的收缩结果，不等大。
//!    - `auto`（默认）：组内参与格的 left / x1 / 高度 极差都 <= 12px 视为
//!      等大卡片（sprite），否则按 free 处理。
//!
//!    参与格 = 边界可信的格子：收缩后 `x0 > 0` 且 `x1 < w` 且 `y1 < h`。
//!    贴源图边的格子（如 4.jpg 底格卡片超出源图、全宽到底）边界不可信，
//!    不参与统计也保持原样——统一它们会裁掉真实内容。
//!
//! 输出：总是同时产出 `<name>-preview.png`（带彩色框线的原图，annotate 复用，
//! 供人工核对）与裁剪的 `<name>-<序号>.png`，写入 `<输入所在目录>/out/`。

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
    let mut min_gap: u32 = 6;
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

        let rects = split_panels(&img, threshold, min_gap);
        eprintln!(
            "[split] {} {}x{} → {} 块",
            src.display(),
            img.width(),
            img.height(),
            rects.len()
        );
        results.push((name, img.width(), img.height(), rects));
    }

    // 组内一致性（按模式）。
    let mode = match mode.as_str() {
        "sprite" => Mode::Sprite,
        "free" => Mode::Free,
        _ => Mode::Auto,
    };
    unify_sizes(&mut results, mode);

    // 输出：预览总是产出 + 裁剪。
    let mut total = 0usize;
    for (name, _w, _h, rects) in &results {
        let src = inputs
            .iter()
            .find(|s| {
                s.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .as_deref()
                    == Some(name.as_str())
            })
            .context("源图丢失")?;
        let img = image::open(src)
            .with_context(|| format!("读取图片失败: {}", src.display()))?
            .to_rgb8();

        // 预览总是产出：带彩框原图，随时可人工核对切分框。
        let widgets: Vec<Widget> = rects
            .iter()
            .enumerate()
            .map(|(idx, r)| Widget {
                id: idx,
                label: String::new(),
                rect: *r,
                color: [255, 0, 0],
                area_ratio: (r.2 * r.3) as f32 / (img.width() * img.height()) as f32,
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

fn bail_usage() -> anyhow::Result<()> {
    anyhow::bail!(
        "用法: split-panels <图片或目录> [--out <dir>] [--threshold N] [--min-gap N] [--mode sprite|free|auto]"
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

/// 组内尺寸一致性（sprite 模式）。
///
/// 各格收缩后的尺寸有 ±几 px 噪声（渐变带内边缘位置的抖动），同源格子会
/// 漂移出 872/868/865 这种渐小尺寸。统一到组内**最紧**：`left = max`、
/// `x1 = min`、`height = min`——每边都裁到组内最深，零白边保证；多裁的
/// 1-4px 是边缘渐变带，无视觉内容。
///
/// 参与格 = 边界可信的格子：收缩后 `x0 > 0` 且 `x1 < w` 且 `y1 < h`。
/// 贴源图边的格子（如 4.jpg 底格卡片超出源图、全宽到底）边界不可信，
/// 不参与统计也保持原样——统一它们会裁掉真实内容。
///
/// 自由模式（Free）跳过统一。自动模式（Auto）先算组内极差再决定。
fn unify_sizes(results: &mut [(String, u32, u32, Vec<(u32, u32, u32, u32)>)], mode: Mode) {
    use std::collections::HashMap;

    let mut groups: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, (_, w, _, _)) in results.iter().enumerate() {
        groups.entry(*w).or_default().push(i);
    }

    for (_, idxs) in groups {
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
                eprintln!("[mode] auto → free (边界极差超 {SPRITE_SPREAD_MAX}px)");
                continue;
            }
        }
        if mode == Mode::Free {
            continue;
        }

        // Sprite：统一到组内最紧（零白边；多裁的是边缘渐变带）。
        let ul = *lefts.iter().max().unwrap();
        let ux1 = *x1s.iter().min().unwrap();
        let uh = *heights.iter().min().unwrap();
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

/// 核心：行投影切行 + 四边收缩。见模块文档。
fn split_panels(img: &RgbImage, threshold: u8, min_gap: u32) -> Vec<(u32, u32, u32, u32)> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    // 每像素最暗通道值：分隔带检测与边缘收缩的聚合信号。
    let darkness: Vec<u8> = img
        .pixels()
        .map(|p| *p.0.iter().min().unwrap())
        .collect();

    // 行投影 → 行块（threshold 恰好分离白边 >=195 与卡片内容 <=180）。
    let row_dark: Vec<u8> = (0..h)
        .map(|y| (0..w).map(|x| darkness[y * w + x]).min().unwrap())
        .collect();

    let mut rects = Vec::new();
    for (y0, y1) in bright_blocks(&row_dark, threshold, min_gap) {
        // 四边收缩：从每条边向内跳过「整行/列 min >= threshold」的浅色边缘，
        // 停在第一个内容行/列（阈值语义，渐变过渡也能正确定位）。
        let y0t = (y0..y1).find(|&y| row_dark[y] < threshold).unwrap_or(y0);
        let y1t = (y0..y1)
            .rev()
            .find(|&y| row_dark[y] < threshold)
            .map(|y| y + 1)
            .unwrap_or(y0t);
        if y1t <= y0t {
            continue;
        }

        let col_dark: Vec<u8> = (0..w)
            .map(|x| (y0t..y1t).map(|y| darkness[y * w + x]).min().unwrap())
            .collect();
        let x0t = (0..w).find(|&x| col_dark[x] < threshold).unwrap_or(0);
        let x1t = (0..w)
            .rev()
            .find(|&x| col_dark[x] < threshold)
            .map(|x| x + 1)
            .unwrap_or(x0t);
        if x1t <= x0t {
            continue;
        }

        rects.push((x0t as u32, y0t as u32, (x1t - x0t) as u32, (y1t - y0t) as u32));
    }
    rects
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
