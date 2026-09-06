//! 命令行：把多格漫画长图切成紧致的独立子图。
//!
//!   cargo run -p ocr-layout --example split-panels -- <图片或目录> [--out <dir>] [--threshold N] [--min-gap N] [--preview]
//!
//! 把「纯图片构成的 UI」交给布局层：每一格就是一条**无文字信号的 Widget**
//! （source=Color）。算法不假设网格整齐排列（样本 4.jpg 的格内就是大间距
//! 非整齐布局），分两步：
//!
//! 1. **行投影**：`row_dark[y]` = 该行最暗像素亮度；整行无内容（亮度 >= 阈值）
//!    的连续行 >= `min_gap` 视为格间分隔带，带间即行块。
//! 2. **列边缘 trim**：行块内从左右边缘向内收缩连续「整列白」，消除残留
//!    白边；只修边、不切中间——内容稀疏的格子（整列背景白）不会被拦腰截断。
//!
//! 之所以不用膨胀 + 连通域：格内元素间距大时半径怎么调都不对（实测 4.jpg
//! 半径 10/20/23 分别切出 7/8/10 块，越调越碎）；行投影靠「整行聚合」对稀疏
//! 布局天然鲁棒。
//!
//! `--preview` 只输出带彩色框线的原图（annotate 复用），供人工核对切分框；
//! 默认输出紧致裁剪的 PNG 到 `<输入所在目录>/out/`。

use anyhow::Context;
use image::RgbImage;
use ocr_layout::{Widget, WidgetSource, annotate};
use std::path::{Path, PathBuf};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        bail_usage()?;
    }

    // 手动解析（对齐 examples/layout.rs 的风格）：input [--out dir]
    // [--threshold N] [--min-gap N] [--preview]
    let mut input: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut threshold: u8 = 220;
    let mut min_gap: u32 = 6;
    let mut preview = false;
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
            "--preview" => {
                preview = true;
                i += 1;
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

    let mut total = 0usize;
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

        if preview {
            // Widget 化走库级 annotate：彩色框 + 无中心十字干扰的判断都交给库。
            // （annotate 会画中心十字——预览时它无害，还能提示「这是一格」。）
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
            let out = out_dir.join(format!("{name}-preview.png"));
            annotate(&img, &widgets)
                .save(&out)
                .with_context(|| format!("保存失败: {}", out.display()))?;
            eprintln!("        预览 → {}", out.display());
            total += rects.len();
            continue;
        }

        for (idx, (x, y, w, h)) in rects.iter().enumerate() {
            let out = out_dir.join(format!("{name}-{}.png", idx + 1));
            crop_rgb(&img, *x, *y, *w, *h)
                .save(&out)
                .with_context(|| format!("保存失败: {}", out.display()))?;
            eprintln!("        {}-{}  rect=({x},{y},{w},{h})", name, idx + 1);
            total += 1;
        }
    }
    eprintln!("[done] 共 {total} 块 → {}", out_dir.display());
    Ok(())
}

fn bail_usage() -> anyhow::Result<()> {
    anyhow::bail!(
        "用法: split-panels <图片或目录> [--out <dir>] [--threshold N] [--min-gap N] [--preview]"
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

/// 核心：行投影切行 + 行块内列边缘 trim（见模块文档）。
fn split_panels(img: &RgbImage, threshold: u8, min_gap: u32) -> Vec<(u32, u32, u32, u32)> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    // 每像素最暗通道值：与纯白的距离决定「算不算内容」。
    let darkness: Vec<u8> = img
        .pixels()
        .map(|p| *p.0.iter().min().unwrap())
        .collect();

    // 行投影 → 行块。
    let row_dark: Vec<u8> = (0..h)
        .map(|y| (0..w).map(|x| darkness[y * w + x]).min().unwrap())
        .collect();

    let mut rects = Vec::new();
    for (y0, y1) in bright_blocks(&row_dark, threshold, min_gap) {
        // 列边缘 trim：只从两端向内收缩，中间不切。
        let x0 = (0..w).find(|&x| col_dark(&darkness, w, x, y0, y1) < threshold).unwrap_or(0);
        let x1 = (0..w)
            .rev()
            .find(|&x| col_dark(&darkness, w, x, y0, y1) < threshold)
            .map(|x| x + 1)
            .unwrap_or(x0);
        if x1 > x0 {
            rects.push((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32));
        }
    }
    rects
}

/// 某列在 `[y0, y1)` 内的最暗像素亮度。
fn col_dark(darkness: &[u8], w: usize, x: usize, y0: usize, y1: usize) -> u8 {
    (y0..y1).map(|y| darkness[y * w + x]).min().unwrap()
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
