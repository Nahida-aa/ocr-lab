//! 屏幕操作层：把「在屏幕绝对坐标上模拟人操作」这件事做成独立、可复用的 crate。
//!
//! 与 `capturer`（看）对称，构成 **看 / 操作分离** 的两条正交链路：
//! - `capturer` 负责「看」——从屏幕/窗口流拿到像素；
//! - `screen-operator` 负责「操作」——把意图（移动、点击、打字…）注入到屏幕。
//!
//! 坐标语义：**屏幕绝对坐标**（以屏幕左上角为原点）。窗口相对坐标 → 绝对坐标的
//! 换算不在本层职责内（那是 `ocr-agent` 的事，它已有 `infer_window_offset`）。
//! 本层只认绝对坐标，因此可被任何「已知道目标屏幕位置」的调用方复用，不耦合
//! 任何窗口模型。
//!
//! 注入后端：当前统一走 `ydotool`（Wayland / 通用 Linux 下的用户态输入注入，
//! 需 `ydotoold` 在跑）。ydotool 本身用绝对坐标，与本层语义天然一致。
//!
//! 已踩坑并固化在本实现里：
//! - 绝对移动必须用 `ydotool mousemove -a -x X -y Y`，**不能**用 `mousemove -- -a X Y`
//!   形式——后者在部分 ydotool 版本会触发 stack smashing（exit 134 崩溃）。
//! - 按键码：`0x40` 表按下、`0x80` 表抬起；左键完整点击 = `0x40|0x00` = `0xC0`
//!   （右 `0xC1`、中 `0xC2`），按下/抬起分离则分别只用 `0x40`/`0x80` 位。
//! - **键盘关键坑**：ydotool `key` **只认数字 keycode，不认 `KEY_*` 名字**
//!   （名字被 strtol 当成 0 静默失效）。本 crate 已内置 [`KEYCODES`] 表把
//!   `KEY_*` 名字翻译成数字码，调用方直接写名字即可。详见
//!   `docs/keycode.md`。

use anyhow::{Context, Result};
use phf::phf_map;

/// 鼠标按键。编码与 ydotool 的键码索引一致（0=左 1=右 2=中 …）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Side,
    Extra,
    Forward,
    Back,
    Task,
}

impl MouseButton {
    /// ydotool 的按键索引位（0x00 左、0x01 右、0x02 中 …）。
    fn index(&self) -> u8 {
        match self {
            MouseButton::Left => 0x00,
            MouseButton::Right => 0x01,
            MouseButton::Middle => 0x02,
            MouseButton::Side => 0x03,
            MouseButton::Extra => 0x04,
            MouseButton::Forward => 0x05,
            MouseButton::Back => 0x06,
            MouseButton::Task => 0x07,
        }
    }
    /// 仅「按下」的键码（down 位 0x40 | 索引）。
    pub fn down_code(&self) -> u8 {
        0x40 | self.index()
    }
    /// 仅「抬起」的键码（up 位 0x80 | 索引）。
    pub fn up_code(&self) -> u8 {
        0x80 | self.index()
    }
    /// 「按下并抬起」的完整点击键码（down|up）。
    pub fn click_code(&self) -> u8 {
        self.down_code() | self.up_code()
    }
}

/// 键盘键名 → Linux 输入子系统 keycode（`/usr/include/linux/input-event-codes.h`）。
///
/// **重要**：ydotool 的 `key` 子命令只认**数字 keycode**（`strtol` 解析），直接传
/// `KEY_ENTER` 这类名字会被当成 0（KEY_RESERVED）静默失效。所以本 crate 内置这张
/// 表，让 `key("KEY_S")` / `combo(&["KEY_LEFTCTRL","KEY_S"])` 这种直观写法能正确
/// 翻译成数字码再透传给 ydotool。`keycode_of` 同时接受：
/// - `KEY_*` 名字（下表涵盖常用键，未列出者可提 PR 补）；
/// - 纯数字字符串（如 `"31"`、`"0x1F"`）直接作为 keycode。
fn keycode_of(name: &str) -> Result<u16> {
    // 纯数字（十进制或 0x 十六进制）直接当 keycode。
    if let Some(hex) = name.strip_prefix("0x").or_else(|| name.strip_prefix("0X")) {
        return u16::from_str_radix(hex, 16).with_context(|| format!("非法十六进制键码: {name}"));
    }
    if name.bytes().all(|b| b.is_ascii_digit()) {
        return name
            .parse::<u16>()
            .with_context(|| format!("非法十进制键码: {name}"));
    }
    KEYCODES
        .get(name.to_ascii_uppercase().as_str())
        .copied()
        .with_context(|| {
            format!(
                "未知键名: {name}（ydotool 只认数字 keycode；可用 KEY_* 名或数字，见 KEYCODES 表）"
            )
        })
}

/// 常用键名 → Linux keycode 映射。键名统一大写（查表时忽略大小写）。
///
/// 数据来源：`linux/input-event-codes.h`（KEY_* 宏值）。仅列常用键，覆盖
/// 字母/数字/修饰键/功能键/方向键/编辑键；特殊键按需补充。
static KEYCODES: phf::Map<&'static str, u16> = phf_map! {
    // 修饰键
    "KEY_LEFTCTRL" => 29, "KEY_RIGHTCTRL" => 97, "KEY_LEFTSHIFT" => 42,
    "KEY_RIGHTSHIFT" => 54, "KEY_LEFTALT" => 56, "KEY_RIGHTALT" => 100,
    "KEY_LEFTMETA" => 125, "KEY_RIGHTMETA" => 126,
    // 字母
    "KEY_A" => 30, "KEY_B" => 48, "KEY_C" => 46, "KEY_D" => 32, "KEY_E" => 18,
    "KEY_F" => 33, "KEY_G" => 34, "KEY_H" => 35, "KEY_I" => 23, "KEY_J" => 36,
    "KEY_K" => 37, "KEY_L" => 38, "KEY_M" => 50, "KEY_N" => 49, "KEY_O" => 24,
    "KEY_P" => 25, "KEY_Q" => 16, "KEY_R" => 19, "KEY_S" => 31, "KEY_T" => 20,
    "KEY_U" => 22, "KEY_V" => 47, "KEY_W" => 17, "KEY_X" => 45, "KEY_Y" => 21,
    "KEY_Z" => 44,
    // 数字（主键盘）
    "KEY_0" => 11, "KEY_1" => 2, "KEY_2" => 3, "KEY_3" => 4, "KEY_4" => 5,
    "KEY_5" => 6, "KEY_6" => 7, "KEY_7" => 8, "KEY_8" => 9, "KEY_9" => 10,
    // 功能键
    "KEY_F1" => 59, "KEY_F2" => 60, "KEY_F3" => 61, "KEY_F4" => 62, "KEY_F5" => 63,
    "KEY_F6" => 64, "KEY_F7" => 65, "KEY_F8" => 66, "KEY_F9" => 67, "KEY_F10" => 68,
    "KEY_F11" => 87, "KEY_F12" => 88,
    // 编辑 / 导航
    "KEY_ENTER" => 28, "KEY_ESC" => 1, "KEY_TAB" => 15, "KEY_SPACE" => 57,
    "KEY_BACKSPACE" => 14, "KEY_DELETE" => 111, "KEY_INSERT" => 110,
    "KEY_HOME" => 102, "KEY_END" => 107, "KEY_PAGEUP" => 104, "KEY_PAGEDOWN" => 109,
    "KEY_LEFT" => 105, "KEY_RIGHT" => 106, "KEY_UP" => 103, "KEY_DOWN" => 108,
    "KEY_CAPSLOCK" => 58, "KEY_NUMLOCK" => 69, "KEY_SCROLLLOCK" => 70,
};

/// 屏幕操作器：在屏幕绝对坐标上注入鼠标 / 键盘输入。
///
/// 轻量、无状态（除后端命令名外），每次调用直接 spawn 一个 `ydotool` 子进程。
/// 若需更高吞吐（连续大量输入）可后续缓存，但当前闭环场景足够了。
#[derive(Debug, Clone, Default)]
pub struct ScreenOperator {
    /// ydotool 可执行文件名（默认 `ydotool`，允许调用方替换为绝对路径）。
    bin: String,
}

impl ScreenOperator {
    /// 用默认的 `ydotool` 后端构造。要求系统已安装 `ydotool` 且 `ydotoold` 在运行
    /// （`systemctl --user enable --now ydotool.service`）。
    pub fn new() -> Self {
        Self {
            bin: "ydotool".to_string(),
        }
    }

    /// 用自定义后端可执行文件构造（如指定 `ydotool` 的绝对路径，或将来换 uinput
    /// 封装）。
    pub fn with_bin(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }

    // ---- 鼠标：移动 ----

    /// 把鼠标指针移动到屏幕绝对坐标 (x, y)。
    pub fn move_to(&self, x: i32, y: i32) -> Result<()> {
        // 注意：必须用 `-a -x X -y Y`，`-a` 表绝对模式。切勿写成 `mousemove -- -a X Y`，
        // 部分 ydotool 版本会 stack smashing 崩溃。
        self.run(&[
            "mousemove",
            "-a",
            "-x",
            &x.to_string(),
            "-y",
            &y.to_string(),
        ])
        .context("ydotool mousemove 失败（确认 ydotool 已安装且 ydotoold 在运行）")
    }

    // ---- 鼠标：点击 ----

    /// 在屏幕绝对坐标 (x, y) 处用 `btn` 键单击一次（按下 + 抬起）。
    pub fn click(&self, x: i32, y: i32, btn: MouseButton) -> Result<()> {
        self.move_to(x, y)?;
        self.run(&["click", &format!("0x{:02X}", btn.click_code())])
            .context("ydotool click 失败")
    }

    /// 左键单击（最常用，便捷封装）。
    pub fn click_left(&self, x: i32, y: i32) -> Result<()> {
        self.click(x, y, MouseButton::Left)
    }

    /// 在屏幕绝对坐标 (x, y) 处双击（左键两次）。
    pub fn double_click(&self, x: i32, y: i32, btn: MouseButton) -> Result<()> {
        self.move_to(x, y)?;
        let code = format!("0x{:02X}", btn.click_code());
        // 两次完整点击，中间用 `-D` 控制间隔（默认即可）。
        self.run(&["click", "-D", "60", &code, &code])
            .context("ydotool 双击失败")
    }

    // ---- 鼠标：按下 / 抬起（用于拖拽）----

    /// 在屏幕绝对坐标 (x, y) 处**按下** `btn` 不抬起（配合 [`ScreenOperator::release`]
    /// 实现拖拽）。
    pub fn press(&self, x: i32, y: i32, btn: MouseButton) -> Result<()> {
        self.move_to(x, y)?;
        self.run(&["click", &format!("0x{:02X}", btn.down_code())])
            .context("ydotool 按下失败")
    }

    /// 在屏幕绝对坐标 (x, y) 处**抬起** `btn`（与 [`ScreenOperator::press`] 配对）。
    pub fn release(&self, x: i32, y: i32, btn: MouseButton) -> Result<()> {
        self.move_to(x, y)?;
        self.run(&["click", &format!("0x{:02X}", btn.up_code())])
            .context("ydotool 抬起失败")
    }

    /// 拖拽：从 `from` 按下 `btn` → 移动到 `to` → 抬起。
    pub fn drag(&self, from: (i32, i32), to: (i32, i32), btn: MouseButton) -> Result<()> {
        self.press(from.0, from.1, btn)?;
        // 拖拽过程里再 move 一次到终点（press 已经 move 到 from，这里 move 到 to）。
        self.move_to(to.0, to.1)?;
        self.release(to.0, to.1, btn)
    }

    // ---- 键盘 ----

    /// 键入一段文本（相当于「粘贴式」输入，不经按键布局展开）。
    /// 适合填表、输命令等；若需模拟真实逐键，用 [`ScreenOperator::key`]。
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.run(&["type", text]).context("ydotool type 失败")
    }

    /// 按一次键（按下 + 抬起）。`name` 接受两种写法：
    /// - `KEY_*` 键名（大小写不敏感），如 `"KEY_ENTER"`、`"KEY_A"`、`"KEY_F5"`、
    ///   `"KEY_LEFTCTRL"`（见 [`KEYCODES`] 表）；
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_codes() {
        assert_eq!(MouseButton::Left.click_code(), 0xC0);
        assert_eq!(MouseButton::Right.down_code(), 0x41);
        assert_eq!(MouseButton::Right.up_code(), 0x81);
        assert_eq!(MouseButton::Middle.click_code(), 0xC2);
    }

    #[test]
    fn keycode_translation() {
        // KEY_* 名字 → 数字 keycode（与 input-event-codes.h 一致）。
        assert_eq!(keycode_of("KEY_ENTER").unwrap(), 28);
        assert_eq!(keycode_of("KEY_LEFTCTRL").unwrap(), 29);
        assert_eq!(keycode_of("KEY_S").unwrap(), 31);
        assert_eq!(keycode_of("key_a").unwrap(), 30); // 大小写不敏感
        // 纯数字直接当 keycode。
        assert_eq!(keycode_of("31").unwrap(), 31);
        assert_eq!(keycode_of("0x1F").unwrap(), 31);
        // 未知名字报错。
        assert!(keycode_of("KEY_NOPE").is_err());
    }
}
