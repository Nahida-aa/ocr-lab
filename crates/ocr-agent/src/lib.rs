//! 业务执行层：把 `ocr-layout` 的控件候选变成对目标应用的真实操作。
//!
//! 本层才真正「理解」语义——把 "点击 Reload" 这种意图翻译成对某个坐标的点击。
//! 它不关心颜色/文字怎么来的（`ocr-layout` 负责），也不关心点击怎么注入
//! （`Executor` 负责，可插拔）。
//!
//! 关键解耦：
//! - **识别**：复用 `ocr_layout::LayoutAnalyzer`，输入截图 → `Widget` 列表。
//! - **执行**：通过 [`Executor`] trait 注入操作。提供
//!   - [`PrintExecutor`]：只打印「将要点击 (x,y)」，用于验证识别→定位链路，
//!     不真正操作（CI / 无显示环境默认用它）。
//!   - [`YdotoolExecutor`]：用 `ydotool` 向 Wayland 注入鼠标点击，需要窗口在
//!     屏幕上的偏移（[`YdotoolExecutor::window_offset`]）把窗口相对坐标转成
//!     绝对坐标。
//!
//! 这样 `ocr-agent` 对目标应用完全透明：不需要 app 暴露任何调试接口，只靠
//! 「看屏幕 + 模拟输入」操控，与人工操作等价。

use anyhow::{Context, Result};
use image::RgbImage;
use ocr_layout::{LayoutAnalyzer, Widget};

/// 操作执行器：把「窗口相对坐标的一次点击」落地。
///
/// 坐标语义统一为**窗口相对像素**（窗口左上角为原点），由具体实现决定如何
/// 转成绝对坐标 / 注入方式。
pub trait Executor {
    /// 在窗口相对坐标 (x, y) 处点击一次。
    fn click_window(&self, x: u32, y: u32) -> Result<()>;
}

/// 空执行器：只打印意图，不真正操作。
///
/// 用于验证「识别 → 定位」是否正确（确认 center 落在按钮上），以及无显示 /
/// 无注入权限的环境。
pub struct PrintExecutor;

impl Executor for PrintExecutor {
    fn click_window(&self, x: u32, y: u32) -> Result<()> {
        println!("[PrintExecutor] 将点击窗口坐标 ({x}, {y})");
        Ok(())
    }
}

/// 基于 `ydotool` 的真实点击执行器（Wayland / 通用 Linux）。
///
/// `ydotool` 使用**屏幕绝对坐标**，而本层统一用窗口相对坐标，因此需提供窗口
/// 在屏幕上的偏移 [`YdotoolExecutor::window_offset`] = (offset_x, offset_y)。
/// 偏移可由 `xdotool getwindowgeometry` 或手动测量得到。
///
/// 点击时序：`ydotool mousemove` 到绝对坐标 → `ydotool click 0xC0`（左键按下
/// 0x40 + 抬起 0x80 = 0xC0）。要求目标窗口在前台且有输入焦点（Wayland 限制）。
pub struct YdotoolExecutor {
    /// 窗口左上角相对屏幕的偏移 (x, y)。
    pub window_offset: (i32, i32),
}

impl YdotoolExecutor {
    pub fn new(window_offset: (i32, i32)) -> Self {
        Self { window_offset }
    }
}

impl Executor for YdotoolExecutor {
    fn click_window(&self, x: u32, y: u32) -> Result<()> {
        let (ox, oy) = self.window_offset;
        let abs_x = ox + x as i32;
        let abs_y = oy + y as i32;
        // 移动到绝对坐标。
        std::process::Command::new("ydotool")
            .args([
                "mousemove",
                "--",
                "-a",
                &abs_x.to_string(),
                &abs_y.to_string(),
            ])
            .status()
            .with_context(|| "执行 ydotool mousemove 失败（确认已安装 ydotool 且有权限）")?;
        // 左键点击（按下 0x40 + 抬起 0x80）。
        std::process::Command::new("ydotool")
            .args(["click", "0xC0"])
            .status()
            .with_context(|| "执行 ydotool click 失败")?;
        Ok(())
    }
}

/// 控件中心（窗口相对坐标），由包围盒算出。
fn center_of(w: &Widget) -> (u32, u32) {
    let (x, y, ww, hh) = w.rect;
    (x + ww / 2, y + hh / 2)
}

/// 按标签从控件列表中挑一个目标控件（纯函数，便于单测）。
///
/// 匹配优先级（不区分大小写）：
/// 1. 精确相等（标签 == target）；
/// 2. 无精确命中时退回子串包含，选面积最大者。
/// 返回 `None` 表示无匹配。
pub fn find_widget_by_label<'a>(widgets: &'a [Widget], target: &str) -> Option<&'a Widget> {
    let t = target.to_lowercase();
    if let Some(w) = widgets
        .iter()
        .find(|w| !w.label.is_empty() && w.label.to_lowercase() == t)
    {
        return Some(w);
    }
    let mut candidates: Vec<&Widget> = widgets
        .iter()
        .filter(|w| {
            let label = w.label.to_lowercase();
            !label.is_empty() && label.contains(&t)
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| b.area_ratio.partial_cmp(&a.area_ratio).unwrap());
    Some(candidates[0])
}

/// 操控代理：识别 + 执行。
pub struct Agent {
    analyzer: LayoutAnalyzer,
    executor: Box<dyn Executor>,
}

impl Agent {
    pub fn new(analyzer: LayoutAnalyzer, executor: Box<dyn Executor>) -> Self {
        Self { analyzer, executor }
    }

    /// 按标签点击一个控件。
    ///
    /// 匹配优先级（均不区分大小写）：
    /// 1. **精确相等**：标签 == target（如 "Load" 精确命中 Load，不会被 "Reload"
    ///    的包含匹配抢走）。
    /// 2. **子串包含**：无精确命中时，才退回「标签包含 target」，且选面积更大的
    ///    （避免点到同名文字碎片）。
    /// 命中后点击其几何中心。
    pub fn click_by_label(&mut self, img: &RgbImage, target: &str) -> Result<()> {
        let widgets = self.analyzer.analyze(img)?;
        let w = find_widget_by_label(&widgets, target)
            .ok_or_else(|| anyhow::anyhow!("未找到标签匹配 '{target}' 的控件"))?;
        Self::dispatch(self.executor.as_ref(), w)
    }

    /// 打印匹配信息并委托执行器点击控件中心。
    fn dispatch(executor: &dyn Executor, w: &Widget) -> Result<()> {
        let (cx, cy) = center_of(w);
        println!(
            "[Agent] 匹配控件 #{}(label={:?}, area={:.2}%) → 点击中心 ({}, {})",
            w.id,
            w.label,
            w.area_ratio * 100.0,
            cx,
            cy
        );
        executor.click_window(cx, cy)
    }

    /// 读取计数器当前值：找标签能解析为整数的控件（如 "0"）。
    ///
    /// 返回 `None` 表示没识别到数字。多次调用可对比验证点击是否生效。
    pub fn read_count(&mut self, img: &RgbImage) -> Result<Option<i32>> {
        let widgets = self.analyzer.analyze(img)?;
        for w in &widgets {
            if let Ok(n) = w.label.trim().parse::<i32>() {
                return Ok(Some(n));
            }
        }
        Ok(None)
    }

    /// 暴露底层 analyzer（如需自定义参数后重新分析）。
    pub fn analyzer(&mut self) -> &mut LayoutAnalyzer {
        &mut self.analyzer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocr_layout::WidgetSource;

    fn mk(id: usize, label: &str, area: f32) -> Widget {
        Widget {
            id,
            label: label.to_string(),
            rect: (0, 0, 10, 10),
            color: [0, 0, 0],
            area_ratio: area,
            source: WidgetSource::Color,
        }
    }

    #[test]
    fn exact_match_beats_substring() {
        // "Load" 应精确命中 Load，而非被 "Reload" 的子串包含抢走。
        let ws = [mk(0, "Reload", 0.05), mk(1, "Load", 0.04)];
        let hit = find_widget_by_label(&ws, "Load").expect("应命中");
        assert_eq!(hit.label, "Load");
    }

    #[test]
    fn substring_fallback_picks_largest() {
        // 没有精确 "load" 时，退回子串包含，选面积更大的 Reload。
        let ws = [mk(0, "Reload", 0.05), mk(1, "Unrelated", 0.01)];
        let hit = find_widget_by_label(&ws, "load").expect("应命中");
        assert_eq!(hit.label, "Reload");
    }

    #[test]
    fn no_match_returns_none() {
        let ws = [mk(0, "Reload", 0.05)];
        assert!(find_widget_by_label(&ws, "Save").is_none());
    }

    #[test]
    fn empty_label_ignored() {
        let ws = [mk(0, "", 0.05), mk(1, "Load", 0.04)];
        let hit = find_widget_by_label(&ws, "load").expect("应命中 Load");
        assert_eq!(hit.label, "Load");
    }
}
