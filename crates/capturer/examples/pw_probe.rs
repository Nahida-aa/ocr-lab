//! 验证示例：用 `ScreenCastCapturer` 选「窗口」+ PipeWire 抽若干帧存 PNG。
//!
//! 仅用来验证 `crates/capturer/src/screencast.rs` 的公开 API 在你的 KDE/Wayland
//! 上可用，且截到的是窗口本体（不受遮挡）。这正是录屏软件「选 app」的能力来源。
//!
//! 默认抓 **两帧**：首帧 + 隔 1 秒的一帧。首帧可能是窗口刚建/刚切到前台时的
//! 过渡态（动画、未渲染完），第二帧通常已稳定，足够默认使用。
//!
//! 运行（在真实 KDE 会话里）：
//!   cargo run -p capturer --example pw_probe -- --out /tmp/win.png
//! 首次会弹 portal 对话框让你选窗口，选完后打印 restore_token，之后可用
//!   --restore <token> 免对话框自动恢复同一选择（即「提前赋权」）。
//!
//! 库代码跑通后，本示例只是薄封装；稳定逻辑在 src/screencast.rs。

use anyhow::Context as _;
use capturer::ScreenCastCapturer;
use std::path::Path;
use std::time::Duration;

/// 把 `out` 变成带序号的文件名：首帧用原名，后续 `out_stem_t{k}.ext`。
fn frame_path(out: &str, k: usize) -> String {
    if k == 0 {
        return out.to_string();
    }
    let p = Path::new(out);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "frame".into());
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "png".into());
    let parent = p
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let name = format!("{}_t{}.{}", stem, k, ext);
    if parent.is_empty() {
        name
    } else {
        format!("{}/{}", parent, name)
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut out = "/tmp/pw_probe.png".to_string();
    let mut restore: Option<String> = None;
    let mut frames: usize = 2; // 默认两帧：首帧 + 1 秒后。
    let mut interval_ms: u64 = 1000; // 帧间隔 1 秒。
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    out = v.clone();
                }
            }
            "--restore" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    restore = Some(v.clone());
                }
            }
            "--frames" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<usize>() {
                        frames = n.max(1);
                    }
                }
            }
            "--interval-ms" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse::<u64>() {
                        interval_ms = n;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    let cap = match &restore {
        Some(t) => ScreenCastCapturer::with_restore_token(t.clone()),
        None => ScreenCastCapturer::new(),
    };

    let mut last_token: Option<String> = None;
    for k in 0..frames {
        // 抽一帧并拿回本次（可能新生成的）restore_token，方便持久化。
        let (img, token) = async_io::block_on(cap.capture_app_token(""))?;
        if let Some(t) = &token {
            last_token = Some(t.clone());
            eprintln!("restore_token = {}", t);
        }
        let path = frame_path(&out, k);
        img.save(&path)
            .with_context(|| format!("保存图片失败: {}", path))?;
        println!(
            "已抽第 {}/{} 帧并存为 {} ({}x{})",
            k + 1,
            frames,
            path,
            img.width(),
            img.height()
        );
        // 最后一帧后不再等待。
        if k + 1 < frames {
            std::thread::sleep(Duration::from_millis(interval_ms));
        }
    }
    if let Some(t) = last_token {
        eprintln!("本次使用的 restore_token = {}", t);
    }
    Ok(())
}
