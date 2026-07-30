//! 验证示例：用 `ScreenCastCapturer` 选「窗口」+ PipeWire 抽一帧存 PNG。
//!
//! 仅用来验证 `crates/capturer/src/screencast.rs` 的公开 API 在你的 KDE/Wayland
//! 上可用，且截到的是窗口本体（不受遮挡）。这正是录屏软件「选 app」的能力来源。
//!
//! 运行（在真实 KDE 会话里）：
//!   cargo run -p capturer --example pw_probe -- --out /tmp/win.png
//! 首次会弹 portal 对话框让你选窗口，选完后打印 restore_token，之后可用
//!   --restore <token> 免对话框自动恢复同一选择（即「提前赋权」）。
//!
//! 库代码跑通后，本示例只是薄封装；稳定逻辑在 src/screencast.rs。

use anyhow::Context as _;
use capturer::ScreenCastCapturer;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut out = "/tmp/pw_probe.png".to_string();
    let mut restore: Option<String> = None;
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
            _ => {}
        }
        i += 1;
    }

    let cap = match &restore {
        Some(t) => ScreenCastCapturer::with_restore_token(t.clone()),
        None => ScreenCastCapturer::new(),
    };

    // 抽一帧并拿回本次（可能新生成的）restore_token，方便持久化。
    let (img, token) = async_io::block_on(cap.capture_app_token(""))?;
    if let Some(t) = token {
        eprintln!("restore_token = {}", t);
    }
    img.save(&out)
        .with_context(|| format!("保存图片失败: {}", out))?;
    println!("已抽帧并存为 {} ({}x{})", out, img.width(), img.height());
    Ok(())
}
