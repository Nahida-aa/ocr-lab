//! 注入原语抽象：把「一步输入操作」打给系统。
//!
//! 与 [`Probe`]（读状态）正交——本 trait 只负责「发指令」，不负责「读结果」。
//! 桌面（ydotool）/ 移动端（adb）/ X11（xdotool）各自实现本 trait，闭环骨架
//! [`Mover`] 只认 `Injector`，不感知具体后端。
//!
//! 为什么分 `Injector` 和 `Probe` 两个独立 trait（而非打包成一个 `ScreenInput`）：
//! 注入通道与读数来源在跨端时**必然解耦**——桌面恰巧 ydotool+KWin 配对，但移动端
//! 注入走 adb、确认走截图，两端没有共同的上层概念。打包成一个 trait 会逼移动端
//! 的 `cursor_pos` 永远返回 `None`、桌面被迫理解 touch，互相拖累。分别抽象更干净。

use anyhow::{Context, Result};
use glam::IVec2;

/// 注入原语：一次「把指针/手指移到当前位置的相对偏移、或在当前位置点一下」。
///
/// 注意语义是**相对/当前位置**的：绝对定位由 [`Mover`] 闭环（读→差→移→确认）负责，
/// 本 trait 只提供最底层的「发一步」能力。
pub trait Injector {
    /// 相对当前位置偏移 `delta`（逻辑像素）。桌面即 ydotool `mousemove -- DX DY`；
    /// 移动端可转成 touch down+move（或直接 swipe 增量）。
    fn move_once(&self, delta: IVec2) -> Result<()>;

    /// 在当前位置单击一次（按下+抬起）。桌面即 ydotool `click`；移动端即 down+up。
    fn click(&self) -> Result<()>;
}

/// ydotool 后端（Linux/Wayland 主流用户态输入注入，需 `ydotoold` 在跑）。
#[derive(Clone)]
pub struct YdotoolInjector {
    /// ydotool 可执行文件名（默认 `ydotool`，允许替换为绝对路径）。
    bin: String,
}

impl YdotoolInjector {
    pub fn new(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }

    /// 执行一条 ydotool 子命令并校验退出码。
    fn run(&self, args: &[&str]) -> Result<()> {
        let status = std::process::Command::new(&self.bin)
            .args(args)
            .status()
            .with_context(|| format!("spawn {} 失败（确认已安装且在 PATH）", self.bin))?;
        if !status.success() {
            anyhow::bail!("ydotool 返回非零退出码 {:?}，参数 {:?}", status, args);
        }
        Ok(())
    }
}

impl Injector for YdotoolInjector {
    fn move_once(&self, delta: IVec2) -> Result<()> {
        // `mousemove -- DX DY`：用 `--` 分隔符，正负增量都能正确解析（负值必须用
        // 此形式，否则 ydotool 会把负号误当选项导致光标乱跳）。
        self.run(&[
            "mousemove",
            "--",
            &delta.x.to_string(),
            &delta.y.to_string(),
        ])
        .context("ydotool 相对移动失败")
    }

    fn click(&self) -> Result<()> {
        // 左键完整点击键码 0xC0（down 0x40 | up 0x80）。
        self.run(&["click", "0xC0"])
            .context("ydotool 当前位置单击失败")
    }
}
