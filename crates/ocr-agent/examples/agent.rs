//! 命令行：演示 `ocr-agent` 对 testing_08 的「识别 → 定位 → 点击」链路，
//! 并演示**自动反推窗口偏移**（无需 xdotool/kdotool，纯图像方法）与**闭环验证**。
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
//!   # 闭环验证：直接对比「点击前 / 点击后」两帧的计数（两帧由你在点击前后各
//!   # 抓一次，如用 pw_probe）。delta 即本次操作带来的计数变化。
//!   cargo run -p ocr-agent --example agent --verify-before <before.png> --verify-after <after.png>
//!
//!   # 真·自动闭环（live，纯全屏方案）：自动截全屏 → 识别 Reload 按钮屏幕绝对
//!   # 坐标 → ydotool 点该绝对坐标 → 再截全屏 → 比 delta。不依赖 restore_token /
//!   # 录屏窗口流；目标窗口由 KdeForegrounder 经 KWin D-Bus 自动切到最前。
//!   # 需：ydotoold 已起（systemctl --user enable --now ydotool.service）、
//!   #    qdbus6 可用（KDE 自带）、testing_08 进程在跑（会被自动提到最前）。
//!   # dry-run（验证识别/定位链路，不真点）：
//!   cargo run -p ocr-agent --example agent --live --label Reload
//!   # 真点（点 Reload 后 count 应 +50）：
//!   cargo run -p ocr-agent --example agent --live --real --label Reload
//!
//! 说明：PrintExecutor 不会真点，所以前后 count 不变；--live --real 用
//! YdotoolExecutor + KdeForegrounder 真点，点 Reload 后 count 应 +50。
//!
//! 注：capturer 的截全屏（Screenshot 接口）是 [dependencies]，自动闭环直接用，
//! 不依赖 opencv（opencv 来自 rapidocr-ort 的 OCR 引擎，已在本机编好）。

use anyhow::Context as _;
use ocr_agent::{
    Agent, Executor, Foregrounder, PrintExecutor, YdotoolExecutor, infer_window_offset,
};
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
        anyhow::bail!(
            "用法: agent <image.png> [--auto-offset --full F --window W] [--real] | agent --verify-before B --verify-after A"
        );
    }

    let mut auto_offset = false;
    let mut full_path: Option<String> = None;
    let mut window_path: Option<String> = None;
    let mut real = false;
    let mut verify_before: Option<String> = None;
    let mut verify_after: Option<String> = None;
    let mut live = false;
    let mut live_label = "Reload".to_string();
    let mut dump = false;

    let mut i = 1; // 从 1 开始，跳过程序名（首参可能是 image 或 --verify-before）
    while i < args.len() {
        match args[i].as_str() {
            "--auto-offset" => auto_offset = true,
            "--full" => full_path = args.get(i + 1).cloned(),
            "--window" => window_path = args.get(i + 1).cloned(),
            "--real" => real = true,
            "--verify-before" => verify_before = args.get(i + 1).cloned(),
            "--verify-after" => verify_after = args.get(i + 1).cloned(),
            "--live" => live = true,
            "--label" => live_label = args.get(i + 1).cloned().unwrap_or_else(|| "Reload".into()),
            "--dump" => dump = true,
            _ => {}
        }
        i += 1;
    }

    // ---- 诊断：raise testing_08 → 截全屏 → OCR 列出所有控件 ----
    // 用来确认「切前台 + 截屏」到底有没有真的抓到 testing_08（而不是被别的窗口盖住）。
    if dump {
        let model_dir = repo_root().join("models/rapidocr");
        let mut analyzer =
            LayoutAnalyzer::with_ocr(ModelProfile::V3, &model_dir, Default::default())
                .context("构建 OCR 引擎失败（确认 models/rapidocr 权重就绪）")?;
        // 先切前台（即使 dry-run 也切，便于诊断）。
        let fg = ocr_agent::KdeForegrounder::new("testing_08");
        fg.raise().context("KdeForegrounder::raise 失败")?;
        std::thread::sleep(std::time::Duration::from_millis(400));
        let rgba = async_io::block_on(capturer::capture_screenshot()).context("截全屏失败")?;
        let img = ocr_agent::rgba_to_rgb(&rgba);
        let widgets = analyzer.analyze(&img)?;
        eprintln!("=== OCR 共识别 {} 个控件 ===", widgets.len());
        for (idx, w) in widgets.iter().enumerate() {
            let (x, y, ww, hh) = w.rect;
            eprintln!(
                "#{} label={:?} area={:.2}% rect=({},{},{},{})",
                idx,
                w.label,
                w.area_ratio * 100.0,
                x,
                y,
                ww,
                hh
            );
        }
        return Ok(());
    }

    // ---- 自动闭环（live，纯全屏方案）----
    // 截全屏 → 识别按钮屏幕绝对坐标 → ydotool 点绝对坐标 → 再截全屏 → 比 delta。
    // 不依赖 restore_token / 录屏窗口流；目标窗口由 KdeForegrounder 自动切前台。
    if live {
        let model_dir = repo_root().join("models/rapidocr");
        let analyzer = LayoutAnalyzer::with_ocr(ModelProfile::V3, &model_dir, Default::default())
            .context("构建 OCR 引擎失败（确认 models/rapidocr 权重就绪）")?;

        // 执行器 + 前台器：--real 才真点 + 自动切前台，否则 dry-run（不切前台也可，
        // 因为不真点；但为验证「看」链路仍建议前台，故 dry-run 也装 Noop 即可）。
        let (executor, foregrounder): (Box<dyn Executor>, Box<dyn ocr_agent::Foregrounder>) =
            if real {
                eprintln!(
                    "使用 YdotoolExecutor（绝对坐标点击）+ KdeForegrounder（自动切前台）真点 {}",
                    live_label
                );
                (
                    Box::new(YdotoolExecutor::new((0, 0))),
                    Box::new(ocr_agent::KdeForegrounder::new("testing_08")),
                )
            } else {
                eprintln!(
                    "使用 PrintExecutor（dry-run，不真点）；加 --real 才会真的点 {}",
                    live_label
                );
                (
                    Box::new(PrintExecutor),
                    Box::new(ocr_agent::NoopForegrounder),
                )
            };

        let mut agent = Agent::with_foregrounder(analyzer, executor, foregrounder);

        let result = async_io::block_on(agent.verify_click_screenshot(&live_label))?;

        println!(
            "闭环(live)：before={:?} after={:?} label={:?} delta={:?}",
            result.before,
            result.after,
            result.label,
            result.delta()
        );
        if result.delta() == Some(50) {
            println!("✓ delta=50，与 testing_08 的 Reload(count+=50) 一致，自动闭环通过");
        } else {
            println!(
                "（delta 非 50：识别偏差 / 点击未生效 / 该按钮语义非 +50；dry-run 下 delta 必然为 0）"
            );
        }
        return Ok(());
    }

    // ---- 闭环验证模式：直接对比两帧计数 ----
    if let (Some(b), Some(a)) = (verify_before, verify_after) {
        let model_dir = repo_root().join("models/rapidocr");
        let analyzer = LayoutAnalyzer::with_ocr(ModelProfile::V3, &model_dir, Default::default())
            .context("构建 OCR 引擎失败（确认 models/rapidocr 权重就绪）")?;
        let mut agent = Agent::new(analyzer, Box::new(PrintExecutor));

        let img_b = image::open(&b)
            .with_context(|| format!("读取 before 图失败: {b}"))?
            .to_rgb8();
        let img_a = image::open(&a)
            .with_context(|| format!("读取 after 图失败: {a}"))?
            .to_rgb8();
        let before = agent.read_count(&img_b)?;
        let after = agent.read_count(&img_a)?;
        let delta = match (before, after) {
            (Some(b), Some(a)) => Some(a - b),
            _ => None,
        };
        println!(
            "闭环验证：before={:?} after={:?} delta={:?}",
            before, after, delta
        );
        if delta == Some(50) {
            println!("✓ delta=50，与 testing_08 的 Reload(count+=50) 一致，闭环通过");
        } else {
            println!("（delta 非 50：可能 before/after 不是 Reload 点击前后，或识别有偏差）");
        }
        return Ok(());
    }

    let image_path = args[1].clone();
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
