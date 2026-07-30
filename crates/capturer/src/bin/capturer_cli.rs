//! capturer 的独立 CLI：不依赖 gpui，纯用 xdg-desktop-portal 抓图。
//!
//! 用法：
//!   抓全屏：           capturer_cli full --out shot.png
//!   抓指定区域：       capturer_cli region 120 120 720 220 --out card.png
//!   从已有全屏图裁切： capturer_cli crop full.png 120 120 720 220 --out card.png
//!
//! 区域坐标为屏幕像素（x y w h）。`crop` 子命令不触发任何抓图，仅对本地
//! 图片做裁切，便于在无显示环境下验证裁切逻辑。

use anyhow::{Context as AnyhowContext, Result};
use capturer::{Capturer, PortalCapturer};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut pos = Vec::new();
    let mut out: PathBuf = PathBuf::from("capture.png");

    // 简单手工解析：第一个位置参数是 subcommand，其余位置参数收集，
    // --out <path> 取输出路径。足够轻量，无需引入 clap。
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--out" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--out 需要一个路径参数");
                }
                out = PathBuf::from(&args[i]);
            }
            other => {
                if other.starts_with("--") {
                    anyhow::bail!("未知选项: {}", other);
                }
                pos.push(other.to_string());
            }
        }
        i += 1;
    }

    if pos.is_empty() {
        print_help();
        return Ok(());
    }

    let cmd = pos.remove(0);
    match cmd.as_str() {
        "full" => cmd_full(&out),
        "region" => cmd_region(&pos, &out),
        "crop" => cmd_crop(&pos, &out),
        other => {
            eprintln!("未知子命令: {}", other);
            print_help();
            std::process::exit(2);
        }
    }
}

fn cmd_full(out: &std::path::Path) -> Result<()> {
    let img = async_io::block_on(async {
        let cap = PortalCapturer::new();
        cap.capture_fullscreen()
            .await
            .context("抓全屏失败（可能需要在桌面环境中授权截图）")
    })?;
    save(&img, out)?;
    println!("已保存全屏 {}x{} -> {}", img.width(), img.height(), out.display());
    Ok(())
}

fn cmd_region(pos: &[String], out: &std::path::Path) -> Result<()> {
    if pos.len() != 4 {
        anyhow::bail!("region 需要 4 个参数：x y w h");
    }
    let (x, y, w, h) = parse_rect(pos)?;
    let img = async_io::block_on(async {
        let cap = PortalCapturer::new();
        cap.capture_region(x, y, w, h)
            .await
            .context("抓区域失败（可能需要在桌面环境中授权截图）")
    })?;
    save(&img, out)?;
    println!(
        "已保存区域 {}x{} (@{},{}）-> {}",
        img.width(),
        img.height(),
        x,
        y,
        out.display()
    );
    Ok(())
}

fn cmd_crop(pos: &[String], out: &std::path::Path) -> Result<()> {
    if pos.len() != 5 {
        anyhow::bail!("crop 需要 5 个参数：<src.png> x y w h");
    }
    let src = &pos[0];
    let rect = &pos[1..];
    let (x, y, w, h) = parse_rect(rect)?;
    let full = capturer::load_rgba(src)
        .with_context(|| format!("读取源图失败: {}", src))?;
    let cropped = capturer::crop_region(&full, x, y, w, h);
    save(&cropped, out)?;
    println!(
        "已从 {} 裁切 {}x{} (@{},{}) -> {}",
        src,
        cropped.width(),
        cropped.height(),
        x,
        y,
        out.display()
    );
    Ok(())
}

fn parse_rect(pos: &[String]) -> Result<(u32, u32, u32, u32)> {
    let parse = |s: &str| -> Result<u32> {
        s.parse::<u32>()
            .with_context(|| format!("无法解析为整数: {}", s))
    };
    Ok((parse(&pos[0])?, parse(&pos[1])?, parse(&pos[2])?, parse(&pos[3])?))
}

fn save(img: &image::RgbaImage, out: &std::path::Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    img.save(out)
        .with_context(|| format!("保存图片失败: {}", out.display()))
}

fn print_help() {
    eprintln!(
        "用法:\n  \
         capturer_cli full  --out <path.png>\n  \
         capturer_cli region <x> <y> <w> <h> --out <path.png>\n  \
         capturer_cli crop <src.png> <x> <y> <w> <h> --out <path.png>"
    );
}
