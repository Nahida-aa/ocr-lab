//! 命令行：演示 `ocr-agent` 对 testing_08 的「识别 → 定位 → 点击」链路。
//!
//!   cargo run -p ocr-agent --example agent -- <image.png> [--real (offset_x,offset_y)]
//!
//! 默认用 `PrintExecutor`（只打印将点的窗口坐标，不真点），用于验证识别与定位。
//! 传 `--real ox,oy` 则改用 `YdotoolExecutor` 真正点击（ox,oy 为 testing_08 窗口
//! 在屏幕上的偏移，需自行用 xdotool 测出）。
//!
//! 说明：本示例用同一张截图演示，PrintExecutor 不会真点，所以前后 count 不变；
//! 换 YdotoolExecutor 且窗口在前台时，点 Reload 后 count 应 +50。

use anyhow::Context as _;
use ocr_agent::{Agent, Executor, PrintExecutor, YdotoolExecutor};
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
        anyhow::bail!("用法: agent <image.png> [--real ox,oy]");
    }
    let image_path = args[1].clone();

    // 解析 --real ox,oy。
    let mut real: Option<(i32, i32)> = None;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--real" {
            if let Some(s) = args.get(i + 1) {
                let parts: Vec<&str> = s.split(',').collect();
                if parts.len() == 2 {
                    if let (Ok(x), Ok(y)) = (parts[0].parse(), parts[1].parse()) {
                        real = Some((x, y));
                    }
                }
                i += 2;
                continue;
            }
        }
        i += 1;
    }

    let img = image::open(&image_path)
        .with_context(|| format!("读取图片失败: {image_path}"))?
        .to_rgb8();

    let model_dir = repo_root().join("models/rapidocr");
    let analyzer = LayoutAnalyzer::with_ocr(ModelProfile::V3, &model_dir, Default::default())
        .context("构建 OCR 引擎失败（确认 models/rapidocr 权重就绪）")?;

    // 选择执行器。
    let executor: Box<dyn Executor> = match real {
        Some((ox, oy)) => {
            eprintln!("使用 YdotoolExecutor（窗口偏移 {},{}）", ox, oy);
            Box::new(YdotoolExecutor::new((ox, oy)))
        }
        None => {
            eprintln!("使用 PrintExecutor（dry-run，不真点）");
            Box::new(PrintExecutor)
        }
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
