//! 命令行：演示 `ocr-agent` 对 testing_08 的「识别 → 定位 → 点击」链路，
//! 并演示**自动反推窗口偏移**（无需 xdotool/kdotool，纯图像方法）。
//!
//! 用法：
//!   # 默认：PrintExecutor（dry-run），手动无偏移。
//!   cargo run -p ocr-agent --example agent -- <image.png>
//!
//!   # 自动反推偏移（离线双图）：用已有的全屏图 + 窗口流图反推屏幕偏移。
//!   cargo run -p ocr-agent --example agent -- <window.png> \
//!       --auto-offset --full <fullscreen.png> --window <window.png>
//!
//!   # 结合真实点击：反推出偏移后用 ydotool 真点。
//!   cargo run -p ocr-agent --example agent -- <window.png> \
//!       --auto-offset --full <full.png> --window <window.png> --real
//!
//! 说明：PrintExecutor 不会真点，所以前后 count 不变；--real 且窗口在前台时，
//! 点 Reload 后 count 应 +50。
//!
//! 注：在线抓取（用 capturer 抓全屏+窗口流再反推）需引入 capturer/async-io
//! （会拉 opencv 重依赖），本示例默认只演示「离线双图反推」，live 抓取请在
//! 本地自行接线（参考 infer_window_offset 的调用方式）。

use anyhow::Context as _;
use ocr_agent::{Agent, Executor, PrintExecutor, YdotoolExecutor, infer_window_offset};
use ocr_layout::LayoutAnalyzer;
use rapidocr_ort::ModelProfile;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let exe = std::env::current_exe().expect("当前可执行文件路径");
    exe.parent()
        .unwrap()
        .join("..") // target/debug
        .join("..") // crates/ocr-agent
        .join("..") // 仓库根
        .canonicalize()
        .expect("解析仓库根失败")
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        anyhow::bail!("用法: agent <image.png> [--auto-offset --full F --window W] [--real]");
    }
    let image_path = args[1].clone();

    let mut auto_offset = false;
    let mut full_path: Option<String> = None;
    let mut window_path: Option<String> = None;
    let mut real = false;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--auto-offset" => auto_offset = true,
            "--full" => full_path = args.get(i + 1).cloned(),
            "--window" => window_path = args.get(i + 1).cloned(),
            "--real" => real = true,
            _ => {}
        }
        i += 1;
    }

    let img = image::open(&image_path)
        .with_context(|| format!("读取图片失败: {image_path}"))?
        .to_rgb8();

    // ---- 自动反推窗口偏移（离线双图：全屏 + 窗口流）----
    let inferred: Option<(i32, i32)> = if auto_offset {
        let (f, w) = match (full_path.clone(), window_path.clone()) {
            (Some(f), Some(w)) => (f, w),
            _ => anyhow::bail!("--auto-offset 需要 --full <全屏图> --window <窗口图>"),
        };
        let full = image::open(&f)
            .with_context(|| format!("读取全屏图失败: {f}"))?
            .to_rgb8();
        let win = image::open(&w)
            .with_context(|| format!("读取窗口图失败: {w}"))?
            .to_rgb8();
        let off = infer_window_offset(&full, &win);
        eprintln!("自动反推偏移（双图）= {:?}", off);
        Some(off)
    } else {
        None
    };

    let model_dir = repo_root().join("models/rapidocr");
    let analyzer = LayoutAnalyzer::with_ocr(ModelProfile::V3, &model_dir, Default::default())
        .context("构建 OCR 引擎失败（确认 models/rapidocr 权重就绪）")?;

    // 选择执行器：--real 时用推断（或手动）偏移，否则 PrintExecutor。
    let executor: Box<dyn Executor> = if real {
        let (ox, oy) = inferred.unwrap_or_else(|| {
            eprintln!("警告：--real 但无 --auto-offset，偏移按 (0,0)，点击可能偏移到屏幕 (0,0)");
            (0, 0)
        });
        eprintln!("使用 YdotoolExecutor（窗口偏移 {},{}）", ox, oy);
        Box::new(YdotoolExecutor::new((ox, oy)))
    } else {
        eprintln!(
            "使用 PrintExecutor（dry-run，不真点）{}",
            inferred
                .map(|o| format!("；推断偏移 {:?}", o))
                .unwrap_or_default()
        );
        Box::new(PrintExecutor)
    };

    let mut agent = Agent::new(analyzer, executor);

    // 1. 读当前计数。
    let before = agent.read_count(&img)?;
    println!("点击前 count = {:?}", before);

    // 2. 按标签点击（演示 Reload / Load）。
    for label in ["Reload", "Load"] {
        if let Err(e) = agent.click_by_label(&img, label) {
            eprintln!("点击 {} 失败: {:#}", label, e);
        }
    }

    // 3. 再读一次（PrintExecutor 未真点，值应不变；YdotoolExecutor 会变化）。
    let after = agent.read_count(&img)?;
    println!("点击后 count = {:?}", after);

    Ok(())
}
