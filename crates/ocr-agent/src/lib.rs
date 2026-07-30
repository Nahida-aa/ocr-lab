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

/// 从「全屏图」+「窗口流图」反推窗口在屏幕上的偏移（纯图像方法，不依赖任何
/// 外部命令 / compositor 接口，纯 Wayland 下也能用）。
///
/// 原理：ScreenCast 的窗口流是「窗口自身合成」（窗口相对坐标，不受遮挡），而
/// 同一时刻抓的全屏 Monitor 流含该窗口在屏幕上的真实位置。两图里窗口像素一致，
/// 于是在全屏图里滑窗搜索与窗口图最相似的块，峰值位置即窗口左上角的屏幕坐标。
///
/// 实现：降采样（默认因子 8）转灰度后用 **整窗 SSD** 滑窗（用整窗而非内缩模板，
/// 否则当贴块大于模板时会有多个位置 SSD 同为 0、无法唯一确定）。返回
/// `(offset_x, offset_y)`（屏幕像素）。
///
/// 这是让 `ocr-agent` 自动知道偏移的关键：无需 xdotool/kdotool（纯 Wayland 下
/// 通常不可用），也不用假设窗口居中。
pub fn infer_window_offset(full: &RgbImage, window: &RgbImage) -> (i32, i32) {
    const DOWN: u32 = 8;
    let full_small = downscale_gray(full, DOWN);
    let win_small = downscale_gray(window, DOWN);

    let (fw, fh) = (full_small.width(), full_small.height());
    let (ww, wh) = (win_small.width(), win_small.height());
    if fw < ww || fh < wh {
        return (0, 0);
    }

    let mut best = (0i32, 0i32);
    let mut best_ssd = u64::MAX;
    for y in 0..=(fh - wh) {
        for x in 0..=(fw - ww) {
            let mut ssd = 0u64;
            for ty in 0..wh {
                for tx in 0..ww {
                    let a = full_small.get_pixel(x + tx, y + ty).0[0] as i32;
                    let b = win_small.get_pixel(tx, ty).0[0] as i32;
                    let d = a - b;
                    ssd += (d * d) as u64;
                }
            }
            if ssd < best_ssd {
                best_ssd = ssd;
                best = (x as i32, y as i32);
            }
        }
    }
    // 升采样回原图坐标（降采样块位置即窗口左上角 * DOWN）。
    (best.0 * DOWN as i32, best.1 * DOWN as i32)
}

/// 降采样并转灰度（取原图 1/DOWN 的每 DOWNd 像素，亮度 = 0.299R+0.587G+0.114B）。
fn downscale_gray(img: &RgbImage, down: u32) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    let nw = (w / down).max(1);
    let nh = (h / down).max(1);
    let mut out = RgbImage::new(nw, nh);
    for y in 0..nh {
        for x in 0..nw {
            let p = img.get_pixel(x * down, y * down).0;
            let g = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) as u8;
            out.put_pixel(x, y, image::Rgb([g, g, g]));
        }
    }
    out
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

/// 闭环验证结果。
#[derive(Debug, Clone)]
pub struct VerifyResult {
    /// 点击前计数。
    pub before: Option<i32>,
    /// 点击后计数。
    pub after: Option<i32>,
    /// 被点击的控件标签。
    pub label: String,
}

impl VerifyResult {
    /// 点击带来的计数变化（点击后 − 点击前）；任一侧未识别到数字则为 None。
    pub fn delta(&self) -> Option<i32> {
        match (self.before, self.after) {
            (Some(b), Some(a)) => Some(a - b),
            _ => None,
        }
    }
}

impl Agent {
    /// 闭环验证：点击某控件后重新抓取一帧，读出点击后的计数，与点击前对比。
    ///
    /// 这是「识别 → 定位 → 点击 → 观测」完整链路的收口：`capture_after` 由调用方
    /// 提供（通常是 `capturer` 重新抓一帧），`ocr-agent` 不直接依赖捕获后端，保持
    /// 解耦。`capture_after` 必须在点击**之后**调用，才能反映操作效果。
    ///
    /// 典型用法（伪代码）：
    /// ```ignore
    /// let after = || -> Result<RgbImage> { Ok(capturer.capture_app_token("")?.0.to_rgb8()) };
    /// let r = agent.verify_click(&img_before, "Reload", after)?;
    /// assert_eq!(r.delta(), Some(50)); // testing_08 的 Reload 使 count += 50
    /// ```
    pub fn verify_click<F>(
        &mut self,
        img_before: &RgbImage,
        label: &str,
        capture_after: F,
    ) -> Result<VerifyResult>
    where
        F: FnOnce() -> Result<RgbImage>,
    {
        let before = self.read_count(img_before)?;
        self.click_by_label(img_before, label)?;
        let img_after = capture_after()?;
        let after = self.read_count(&img_after)?;
        Ok(VerifyResult {
            before,
            after,
            label: label.to_string(),
        })
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

    #[test]
    fn verify_result_delta() {
        // 正常：点击后 − 点击前。
        let r = VerifyResult {
            before: Some(0),
            after: Some(50),
            label: "Reload".to_string(),
        };
        assert_eq!(r.delta(), Some(50));

        // 任一侧未识别到数字 → None。
        let r = VerifyResult {
            before: None,
            after: Some(50),
            label: "Reload".to_string(),
        };
        assert_eq!(r.delta(), None);

        // 计数器下降也是正常差值（如 Decrement 按钮）。
        let r = VerifyResult {
            before: Some(10),
            after: Some(9),
            label: "Decrement".to_string(),
        };
        assert_eq!(r.delta(), Some(-1));
    }

    #[test]
    fn infer_offset_recovers_planted_window() {
        use image::Rgb;
        // 全屏 200×160，底色灰；在 (40, 48)（8 的整数倍，便于降采样精确还原）
        // 处贴一块 80×60 的**带纹理**窗。关键是：全屏里的贴块必须是窗口块的「逐
        // 像素拷贝」（用窗口相对坐标算纹理），否则平移后图案不严格相等，匹配会在
        // 多个位置都接近。纯色窗也会因 SSD=0 多处重合而无法唯一确定。
        let off_x = 40u32;
        let off_y = 48u32;
        let tex = |x: u32, y: u32| -> u8 { ((x * 3 + y * 7) % 200) as u8 };

        let mut full = RgbImage::from_pixel(200, 160, Rgb([80, 80, 80]));
        for wy in 0..60u32 {
            for wx in 0..80u32 {
                let fx = off_x + wx;
                let fy = off_y + wy;
                let v = tex(wx, wy);
                full.put_pixel(fx, fy, Rgb([v, v, 230]));
            }
        }
        // 窗口流 = 同一带纹理块（窗口相对坐标）。
        let mut window = RgbImage::from_pixel(80, 60, Rgb([0, 0, 0]));
        for wy in 0..60u32 {
            for wx in 0..80u32 {
                let v = tex(wx, wy);
                window.put_pixel(wx, wy, Rgb([v, v, 230]));
            }
        }
        let (ox, oy) = infer_window_offset(&full, &window);
        assert_eq!((ox, oy), (40, 48), "应反推出窗口真实偏移");
    }
}
