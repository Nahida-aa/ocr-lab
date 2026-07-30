//! 用 gpui 真实渲染文字，再通过 capturer 抓图，生成 OCR 测试 fixture。
//!
//! 为什么用 gpui 生成而不是 PIL：
//! - gpui 走的是真实 GPU 文字渲染（wgpu/Vulkan），画出来的字和真实 GUI 应用
//!   完全一致，比 PIL 假图更贴近「屏幕分析」要面对的真实场景。
//! - 同时把「渲染 → 抓图 → 存 PNG」这条链路跑通，这正是将来模拟操作（在任意
//!   窗口管理器里抓屏 → OCR → 断言）要复用的基础设施。
//!
//! 两种运行模式：
//! - `--gui <fixture>`   ：仅启动一个 gpui 窗口渲染卡片，**不抓图、不存盘**，窗口
//!   常驻若干秒（看门狗）后退出，方便你肉眼检查渲染效果 / 手动截图核对。
//! - `--capture <fixture>`（默认）：渲染后调用 capturer（xdg-desktop-portal
//!   **ScreenCast** + PipeWire）抓取 gpui 窗口本体（不受遮挡）、裁切到卡片区域、
//!   存 PNG，然后干净退出。
//!
//! 关于「提前赋权」：首次 capture 会弹 portal 对话框让你选 gpui 窗口，之后会拿到
//! 一个 `restore_token` 并持久化到 `tests/fixtures/.capture_token`；后续运行自动
//! 复用该 token，免对话框直接选回同一窗口。也可用 `--token <t>` 显式传入，或
//! `--reset-token` 强制重新选窗。
//!
//! 无参数运行：批处理全部 fixture，走 capture 模式。

use anyhow::{Context as AnyhowContext, Result};
use capturer::ScreenCastCapturer;
use gpui::{
    div, px, rgb, size, point, App, Bounds, Context, IntoElement, Render, SharedString, Window,
    WindowBounds, WindowOptions,
};
use gpui::prelude::*;
use gpui_platform::application;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 窗口（即卡片本身）尺寸：卡片内容 720×220 + 四周 20px padding = 760×260。
/// 现在抓的是「窗口本体」，窗口本身就是要识别的卡片，无需再裁切。
const WIN_W: u32 = 760;
const WIN_H: u32 = 260;
/// 卡片内边距（窗口里文字离边框的距离）。
const PAD: f32 = 20.0;

/// restore_token 持久化路径（用于「提前赋权」：首次选窗后存下，之后免对话框）。
const TOKEN_PATH: &str = "tests/fixtures/.capture_token";

/// 运行模式：仅 GUI（不抓图）或 GUI + capturer 抓图存盘。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 仅启动 gpui 窗口渲染，不抓图。
    Gui,
    /// 渲染后调用 capturer 抓图存 PNG。
    Capture,
}

/// 单个 fixture：文件名 + 要渲染的文本行。
struct Fixture {
    name: &'static str,
    lines: Vec<&'static str>,
}

/// 全部内置 fixture（与旧 PIL 生成器对齐，便于对比）。
fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture { name: "ui_stable1", lines: vec!["Count: 100"] },
        Fixture { name: "ui_big1", lines: vec!["Hello OCR"] },
        Fixture { name: "ui_nat1", lines: vec!["Hello World", "RapidOCR test"] },
        Fixture { name: "ui_zh1", lines: vec!["你好世界", "RapidOCR 测试"] },
        Fixture { name: "ui_mix1", lines: vec!["订单编号：A1024", "总金额 ¥128.50", "Status: 已发货"] },
    ]
}

/// 渲染一张「卡片即窗口」的视图：窗口本身就是带边框、内边距的文字卡片，
/// 没有多余白底。抓窗口本体后直接存盘，无需裁切。
struct CardView {
    lines: Vec<SharedString>,
}

impl Render for CardView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0xf2f2f2))
            .border_1()
            .border_color(rgb(0xcccccc))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(8.0))
            .items_start()
            .p(px(PAD))
            .text_size(px(36.0))
            .text_color(rgb(0x111111))
            .children(self.lines.iter().map(|l| div().child(l.clone())))
    }
}

/// 生成单个 fixture。
/// - Gui 模式：只渲染窗口，不抓图；窗口常驻到看门狗超时后退出（便于肉眼核对）。
/// - Capture 模式：渲染后开线程抓图裁切存 PNG，done 置位后干净退出。
///
/// `capture_token`：ScreenCast 的 restore_token（来自文件或 `--token`）。`None`
/// 表示用文件里的 token；若文件也没有，则首次选窗并自动持久化新 token。
/// `reset_token`：忽略文件里的 token，强制重新选窗。
fn generate_one(
    fx: &Fixture,
    out_dir: &std::path::Path,
    mode: Mode,
    capture_token: Option<String>,
    reset_token: bool,
) -> Result<()> {
    let lines: Vec<SharedString> = fx.lines.iter().map(|s| (*s).into()).collect();
    let name = fx.name.to_string();
    let out_path = out_dir.join(format!("{}.png", fx.name));

    application().run(move |cx: &mut App| {
        let view = cx.new(|_| CardView {
            lines: lines.clone(),
        });

        // 窗口本身就是卡片：固定尺寸，放在屏幕左上偏移处（可见且方便 portal 选窗）。
        let bounds = Bounds::new(
            point(px(200.0), px(200.0)),
            size(px(WIN_W as f32), px(WIN_H as f32)),
        );

        let _window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    show: true,
                    focus: true,
                    ..Default::default()
                },
                |_, _| view,
            )
            .unwrap();
        cx.activate(true);

        // done 标志：抓图（或 GUI 停留）完成置位，主轮询据此干净退出。
        let done = Arc::new(AtomicBool::new(false));
        let done_capture = done.clone();

        match mode {
            Mode::Capture => {
                // 在独立线程里抓图（ashpd/portal 走 async-io 后端，用
                // async_io::block_on 驱动；与 gpui 事件循环完全隔离）。
                let out_path2 = out_path.clone();
                let name2 = name.clone();
                // 解析本次使用的 restore_token：--token 优先；否则文件；否则 None（首次选窗）。
                let token_for_run = if reset_token {
                    None
                } else {
                    capture_token
                        .clone()
                        .or_else(|| std::fs::read_to_string(TOKEN_PATH).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
                };
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(800));
                    let result: Result<()> = async_io::block_on(async {
                        let cap = match &token_for_run {
                            Some(t) => ScreenCastCapturer::with_restore_token(t.clone()),
                            None => ScreenCastCapturer::new(),
                        };
                        // 抓 gpui 窗口本体（不受遮挡），并取回可能新生成的 token。
                        let (img, new_token) = cap
                            .capture_app_token("")
                            .await
                            .context("抓图失败（可能需要在桌面环境中授权选窗）")?;
                        // 持久化新 token（首次选窗或 token 轮换时），实现「提前赋权」。
                        if let Some(t) = &new_token {
                            if let Err(e) = std::fs::write(TOKEN_PATH, t) {
                                eprintln!("警告：保存 restore_token 失败: {}", e);
                            } else {
                                eprintln!("已保存 restore_token 到 {}（下次免对话框）", TOKEN_PATH);
                            }
                        }
                        // 窗口本身就是卡片，无需裁切，直接存盘。
                        img.save(&out_path2)
                            .with_context(|| format!("保存图片失败: {}", out_path2.display()))?;
                        Ok(())
                    });
                    match &result {
                        Ok(()) => println!("生成 {} -> {}", name2, out_path2.display()),
                        Err(e) => eprintln!("生成 {} 失败: {:#}", name2, e),
                    }
                    done_capture.store(true, Ordering::SeqCst);
                });
            }
            Mode::Gui => {
                // 仅 GUI：不抓图，窗口常驻 GUI_DWELL 秒供肉眼核对，然后退出。
                println!("[gui] 仅渲染模式：窗口将停留 {}s 后自动退出", GUI_DWELL.as_secs());
                std::thread::spawn(move || {
                    std::thread::sleep(GUI_DWELL);
                    done_capture.store(true, Ordering::SeqCst);
                });
            }
        }

        // 看门狗：双保险。若上面的 done 因任何原因（如 GUI 模式窗口根本没呈现、
        // 或 portal 授权卡住）没能置位，也保证进程在 WATCHDOG 内硬退出。
        let done_wd = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(WATCHDOG);
            if !done_wd.load(Ordering::SeqCst) {
                eprintln!(
                    "看门狗触发：{} 模式在 {}s 内未正常结束，强制退出",
                    if mode == Mode::Gui { "gui" } else { "capture" },
                    WATCHDOG.as_secs()
                );
                std::process::exit(0);
            }
        });

        // 主轮询：done 置位则干净退出 gpui。用 cx.spawn 在「主线程执行器」上轮询，
        // 不依赖窗口是否真正呈现帧（配合 wayland 特性开启后窗口会正常呈现）。
        let done_poll = done.clone();
        let _task = cx.spawn(async move |cx: &mut gpui::AsyncApp| {
            loop {
                if done_poll.load(Ordering::SeqCst) {
                    break;
                }
                async_io::Timer::after(Duration::from_millis(100)).await;
            }
            cx.update(|app| app.quit());
        });
    });

    Ok(())
}

/// 仅 GUI 模式下窗口常驻时间（秒）。
const GUI_DWELL: Duration = Duration::from_secs(10);
/// 看门狗硬超时（秒），双保险防止进程挂起。
const WATCHDOG: Duration = Duration::from_secs(25);

fn main() -> Result<()> {
    let out_dir = std::path::Path::new("tests/fixtures");
    std::fs::create_dir_all(out_dir).ok();

    // 参数解析：
    //   gen_ui_img                 -> 批处理全部 fixture（capture 模式）
    //   gen_ui_img <fixture>       -> 单个 fixture（capture 模式，兼容旧调用）
    //   gen_ui_img --gui <fx>      -> 单个 fixture（仅 GUI）
    //   gen_ui_img --capture <fx>  -> 单个 fixture（抓图存盘）
    let args: Vec<String> = std::env::args().collect();
    let mut mode = Mode::Capture;
    let mut fixture_arg: Option<String> = None;
    let mut token_arg: Option<String> = None;
    let mut reset_token = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--gui" => {
                mode = Mode::Gui;
                i += 1;
                if i < args.len() && !args[i].starts_with("--") {
                    fixture_arg = Some(args[i].clone());
                }
            }
            "--capture" => {
                mode = Mode::Capture;
                i += 1;
                if i < args.len() && !args[i].starts_with("--") {
                    fixture_arg = Some(args[i].clone());
                }
            }
            "--token" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    token_arg = Some(v.clone());
                }
            }
            "--reset-token" => {
                reset_token = true;
            }
            other => {
                if other.starts_with("--") {
                    anyhow::bail!("未知选项: {}", other);
                }
                fixture_arg = Some(other.to_string());
            }
        }
        i += 1;
    }

    // gpui 的 application() 是单例，run 多次不可靠；批处理时每个 fixture 单独起
    // 一个进程生成（重新 exec 自身），保证每次都是干净的 gpui 应用。
    if fixture_arg.is_none() {
        let exe = std::env::current_exe().context("获取当前可执行文件失败")?;
        for fx in fixtures() {
            let mut cmd = std::process::Command::new(&exe);
            cmd.arg("--capture").arg(fx.name);
            if let Some(t) = &token_arg {
                cmd.arg("--token").arg(t);
            }
            if reset_token {
                cmd.arg("--reset-token");
            }
            let status = cmd
                .status()
                .with_context(|| format!("启动子进程生成 {} 失败", fx.name))?;
            if !status.success() {
                anyhow::bail!("生成 {} 失败（退出码 {:?}）", fx.name, status.code());
            }
        }
        println!("全部 fixture 生成完毕 -> {}", out_dir.display());
        return Ok(());
    }

    let target = fixture_arg.unwrap();
    let fx = fixtures()
        .into_iter()
        .find(|f| f.name == target)
        .with_context(|| format!("未知 fixture: {}", target))?;
    generate_one(&fx, out_dir, mode, token_arg, reset_token)
}
