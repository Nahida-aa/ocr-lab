//! 屏幕操作器核心：在屏幕绝对坐标上注入鼠标 / 键盘输入。
//!
//! 分层（详见 [`injector`] / [`probe`] / [`mover`]）：
//! - [`injector::Injector`]：发「相对移一步 / 当前点一下」的原语（ydotool 等）
//! - [`probe::Probe`]：读「当前指针在哪」（KWin 等）
//! - [`mover::Mover`]：**后端无关**的闭环骨架（读→差→移→确认），不认识具体后端
//! - 本 `ScreenOperator` 是**桌面组合方便层**：把 `YdotoolInjector` + `KwinProbe`
//!   拼进 `Mover`，对外只暴露 `move_to(IVec2)` / `click_left_at(IVec2)` 这类直觉 API。
//!   调用方无需感知底层是 ydotool 还是别的。

use crate::ensure_ydotool_flat;
use anyhow::{Context, Result};
use glam::IVec2;

use crate::injector::YdotoolInjector;
use crate::keycode::keycode_of;
use crate::mouse::MouseButton;
use crate::mover::Mover;
use crate::probe::KwinProbe;

/// 屏幕操作器（桌面组合层）：持有「闭环 Mover」+ 键盘注入所需的 ydotool bin。
///
/// 移动/点击走 `mover`（ydotool 相对移动闭环，绕开本机失效的绝对移动）；键盘直接
/// spawn ydotool。坐标语义统一为 **KWin 逻辑坐标**（与 `cursor_pos` / `screen_logical_size`
/// 同套，本机 1800×1125）；物理↔逻辑换算在「看→操作」边界做，不塞进本层。
#[derive(Clone)]
pub struct ScreenOperator {
    /// 闭环移动器（泛型固化为桌面后端：ydotool 注入 + KWin 读数）。
    mover: Mover<YdotoolInjector, KwinProbe>,
    /// ydotool 可执行文件名（键盘注入 / 绝对模式回退用）。
    bin: String,
}

impl std::fmt::Debug for ScreenOperator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenOperator")
            .field("bin", &self.bin)
            .finish()
    }
}

impl Default for ScreenOperator {
    fn default() -> Self {
        Self {
            // 默认后端：ydotool + KWin（KDE/Wayland 桌面）。无 target 的 KwinProbe 仅用于
            // 占位默认；实际用前应通过 with_foregrounder 重组装出带 target 的 probe。
            mover: Mover::new(
                YdotoolInjector::new("ydotool"),
                KwinProbe::new(crate::foregrounder::KdeForegrounder::new("")),
            ),
            bin: "ydotool".to_string(),
        }
    }
}

impl ScreenOperator {
    /// 用默认的 `ydotool` 后端构造。要求系统已安装 `ydotool` 且 `ydotoold` 在运行
    /// （`systemctl --user enable --now ydotool.service`）。
    ///
    /// 注意：默认构造不含有效的 KWin 读数（target 为空），移动/点击闭环需在调用前
    /// 用 [`with_foregrounder`] 组装目标窗口，否则 `Probe` 读不到光标。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用自定义后端可执行文件构造（如指定 `ydotool` 的绝对路径，或将来换 uinput 封装）。
    pub fn with_bin(bin: impl Into<String>) -> Self {
        Self {
            bin: bin.into(),
            ..Self::default()
        }
    }

    /// 用 [`KdeForegrounder`] 组装桌面闭环：注入用 ydotool、读数用该 fg 的 `cursor_pos`。
    /// `cursor_pos` 返回的就是逻辑坐标，与本层入口语义一致。这是 KDE/Wayland 下最常用
    /// 的入口；非 KDE 平台可手动构造各自的 `Injector`+`Probe` 再 `Mover::new`。
    pub fn with_foregrounder(mut self, fg: crate::foregrounder::KdeForegrounder) -> Self {
        self.mover = Mover::new(YdotoolInjector::new(self.bin.clone()), KwinProbe::new(fg));
        self
    }

    /// 链式覆盖底层闭环的单步上限（默认 200 逻辑像素/轴）。透传给
    /// [`Mover::with_step_cap`]——调大可加快大距离移动（但别超过 ~400 实测安全线），
    /// 调小更保守。仅影响 `move_to` / `click_at` 内部的拆步粒度。
    pub fn with_step_cap(mut self, cap: i32) -> Self {
        self.mover = self.mover.with_step_cap(cap);
        self
    }

    /// 链式覆盖底层闭环的到达容差（默认 2 逻辑像素/轴）。透传给
    /// [`Mover::with_tolerance`]——调小要求更精准才停（可能多耗几步纠脏读抖动），
    /// 调大更早停（更快但落点可能偏几像素）。
    pub fn with_tolerance(mut self, tolerance: i32) -> Self {
        self.mover = self.mover.with_tolerance(tolerance);
        self
    }

    // ---- 鼠标：移动 ----

    /// 把鼠标指针移动到屏幕绝对坐标 (x, y)（**绝对模式 `-a`**）。
    ///
    /// 仅作为绝对模式回退 API 暴露给外部；本机 KWin/Wayland 下绝对模式通常失效，
    /// 移动请优先用 [`move_to`]（相对闭环）。
    pub fn move_to_absolute(&self, pos: IVec2) -> Result<()> {
        // 注意：必须用 `-a -x X -y Y`，`-a` 表绝对模式。切勿写成 `mousemove -- -a X Y`，
        // 部分 ydotool 版本会 stack smashing 崩溃。
        self.run(&[
            "mousemove",
            "-a",
            "-x",
            &pos.x.to_string(),
            "-y",
            &pos.y.to_string(),
        ])
        .context("ydotool mousemove 失败（确认 ydotool 已安装且 ydotoold 在运行）")
    }

    /// **确保**移动鼠标到逻辑坐标 `pos`（不点击）。
    ///
    /// 委托给 [`Mover::move_to`]：反复「读当前 → 算差值 → 发相对一步 → 等落盘」直至
    /// 偏差 ≤ 容差。靠每步确认收敛，不预设任何倍率常数——ydotool 相对移动落点不稳定
    /// （单步量不固定、大指令过冲甚至撞墙），只能靠每步确认。
    ///
    /// **坐标语义**：`pos` 为 **KWin 逻辑坐标**（与 `cursor_pos` 返回值同套，本机
    /// 1800×1125）。
    pub fn move_to(&self, pos: IVec2) -> Result<()> {
        // 移动前幂等确保 ydotool 虚拟设备加速度为 flat（KWin 设备级 D-Bus）。
        // 失败静默忽略：未关只令闭环每步效率略低，不影响最终正确性。详见 accel.rs。
        let _ = ensure_ydotool_flat();
        self.mover.move_to(pos)
    }

    /// 原语：相对当前位置移一步 `delta`（逻辑像素），**不读回确认、不闭环**。
    ///
    /// 透传给 [`Mover::move_once`]（再透传到 [`Injector::move_once`]）。供外部直接调用
    /// 「移动一次」原语——例如验证 ydotool 注入本身、或实现自定义逼近。绝大多数业务
    /// 应走 [`move_to`]（闭环、每步确认落点），不要单独用本方法，单次不保证落点准。
    pub fn move_once(&self, delta: IVec2) -> Result<()> {
        self.mover.move_once(delta)
    }

    /// 在逻辑坐标 `pos` 处用 `btn` 键单击一次（按下 + 抬起）。
    ///
    /// 委托给 [`Mover::click_at`]：先闭环移动到 `pos`，再原地点击，绕开本机失效的
    /// ydotool 绝对移动（`mousemove -a`）。
    ///
    /// **坐标语义**：`pos` 为逻辑坐标（与 `cursor_pos` 同套）。
    pub fn click_at(&self, pos: IVec2, btn: MouseButton) -> Result<()> {
        self.mover.click_at(pos)?;
        // 注意：上面的 click_at 已完成移动+左键点击；若 btn 非左键，这里补一条针对性点击。
        // 当前 Mover::click_at 固定用左键（0xC0），非左键场景极少，简单再发一次对应键码。
        if btn != MouseButton::Left {
            self.run(&["click", &format!("0x{:02X}", btn.click_code())])
                .context("ydotool 点击失败")?;
        }
        Ok(())
    }

    /// 左键单击（最常用，便捷封装）。
    pub fn click_left_at(&self, pos: IVec2) -> Result<()> {
        self.click_at(pos, MouseButton::Left)
    }

    /// 在屏幕绝对逻辑坐标 `pos` 处双击（左键两次）。
    ///
    /// 先经 `mover` 闭环移动到 `pos`，再在**当前位置**双击（`double_click_current`）。
    /// 不再依赖本机失效的绝对移动（`mousemove -a`）。
    pub fn double_click(&self, pos: IVec2, btn: MouseButton) -> Result<()> {
        self.move_to(pos)?;
        self.double_click_current(btn)
    }

    /// 在**当前鼠标位置**单击（不移动鼠标）。用于验证执行器注入本身是否生效
    /// （如用户在编辑器里把光标放到某处，原地点一下看是否有响应）。
    pub fn click_current(&self, btn: MouseButton) -> Result<()> {
        self.run(&["click", &format!("0x{:02X}", btn.click_code())])
            .context("ydotool 当前位置单击失败")
    }

    /// 在**当前鼠标位置**双击（不移动鼠标）。
    pub fn double_click_current(&self, btn: MouseButton) -> Result<()> {
        let code = format!("0x{:02X}", btn.click_code());
        self.run(&["click", "-D", "60", &code, &code])
            .context("ydotool 当前位置双击失败")
    }

    // ---- 鼠标：按下 / 抬起（用于拖拽）----

    /// 在屏幕绝对逻辑坐标 `pos` 处**按下** `btn` 不抬起（配合 [`ScreenOperator::release`]
    /// 实现拖拽）。先经 `mover` 闭环移动到 `pos`，再在**当前位置**按下。
    pub fn press(&self, pos: IVec2, btn: MouseButton) -> Result<()> {
        self.move_to(pos)?;
        self.run(&["click", &format!("0x{:02X}", btn.down_code())])
            .context("ydotool 按下失败")
    }

    /// 在屏幕绝对逻辑坐标 `pos` 处**抬起** `btn`（与 [`ScreenOperator::press`] 配对）。
    /// 先经 `mover` 闭环移动到 `pos`，再在**当前位置**抬起。
    pub fn release(&self, pos: IVec2, btn: MouseButton) -> Result<()> {
        self.move_to(pos)?;
        self.run(&["click", &format!("0x{:02X}", btn.up_code())])
            .context("ydotool 抬起失败")
    }

    /// 拖拽：从 `from` 闭环移动到并按下 `btn` → 闭环移动到 `to` → 抬起。
    pub fn drag(&self, from: IVec2, to: IVec2, btn: MouseButton) -> Result<()> {
        self.press(from, btn)?;
        // press 已闭环移动到 from；这里再闭环移动到 to 后抬起。
        self.move_to(to)?;
        self.release(to, btn)
    }

    // ---- 键盘 ----

    /// 键入一段文本（相当于「粘贴式」输入，不经按键布局展开）。
    /// 适合填表、输命令等；若需模拟真实逐键，用 [`ScreenOperator::key`]。
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.run(&["type", text]).context("ydotool type 失败")
    }

    /// 按一次键（按下 + 抬起）。`name` 接受两种写法：
    /// - `KEY_*` 键名（大小写不敏感），如 `"KEY_ENTER"`、`"KEY_A"`、`"KEY_F5"`、
    ///   `"KEY_LEFTCTRL"`（见 [`keycode_of`]）；
    /// - 纯数字 keycode（十进制或 `0x` 十六进制），如 `"31"`、`"0x1F"`。
    ///
    /// 内部把名字翻译成 Linux keycode 数字，再以 `CODE:1 CODE:0` 形式发给 ydotool
    /// （ydotool 的 `key` 只认数字码，直接透传 `KEY_*` 名字会被当成 0 静默失效）。
    pub fn key(&self, name: &str) -> Result<()> {
        let code = keycode_of(name)?;
        self.run(&["key", &format!("{code}:1"), &format!("{code}:0")])
            .context("ydotool key 失败")
    }

    /// 按下某键不抬起（键名写法同 [`ScreenOperator::key`]），配合
    /// [`ScreenOperator::key_up`] 实现组合键（如 Shift+A = key_down("KEY_SHIFT") +
    /// key("KEY_A") + key_up("KEY_SHIFT")）。更省事用 [`ScreenOperator::combo`]。
    pub fn key_down(&self, name: &str) -> Result<()> {
        let code = keycode_of(name)?;
        self.run(&["key", &format!("{code}:1")])
            .context("ydotool key down 失败")
    }

    /// 抬起某键（与 [`ScreenOperator::key_down`] 配对）。键名写法同
    /// [`ScreenOperator::key`]。
    pub fn key_up(&self, name: &str) -> Result<()> {
        let code = keycode_of(name)?;
        self.run(&["key", &format!("{code}:0")])
            .context("ydotool key up 失败")
    }

    /// 直接按数字 keycode（不做名字翻译），按下 + 抬起。适合表外特殊键。
    pub fn key_code(&self, code: u16) -> Result<()> {
        self.run(&["key", &format!("{code}:1"), &format!("{code}:0")])
            .context("ydotool key 失败")
    }

    /// 发送组合键，如 `combo(&["KEY_LEFTCTRL", "KEY_S"])` 即 Ctrl+S。
    ///
    /// 键名写法同 [`ScreenOperator::key`]（`KEY_*` 名或数字码），内部先翻译成
    /// 数字 keycode，再把「依次按下 + 逆序抬起」拼成**一条 ydotool `key` 命令**
    /// 一次发出（时序由 ydotool 自身保证）。等价于：
    /// `ydotool key 29:1 31:1 31:0 29:0`（Ctrl+S 的数字码）。
    pub fn combo(&self, keys: &[&str]) -> Result<()> {
        if keys.is_empty() {
            anyhow::bail!("combo 需要至少一个键名");
        }
        let codes: Vec<u16> = keys
            .iter()
            .map(|k| keycode_of(k))
            .collect::<Result<Vec<_>>>()?;
        let mut seq: Vec<String> = Vec::with_capacity(codes.len() * 2);
        for c in &codes {
            seq.push(format!("{c}:1"));
        }
        for c in codes.iter().rev() {
            seq.push(format!("{c}:0"));
        }
        let args: Vec<&str> = std::iter::once("key")
            .chain(seq.iter().map(|s| s.as_str()))
            .collect();
        self.run(&args).context("ydotool 组合键失败")
    }

    // ---- 底层 ----

    /// 执行一次 ydotool 子命令，校验退出码。
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
