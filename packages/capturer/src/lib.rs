//! 屏幕/窗口抓图基础设施。
//!
//! 这是「模拟操作」链路的核心能力之一：将来在任意系统、任意窗口管理器里
//! 都要把屏幕/窗口抓成图，喂给 OCR 做断言。与其依赖各环境各异的外部命令
//! （spectacle / grim / gnome-screenshot / import），不如自己用 Rust 掌握这条
//! 链路，按运行环境选择后端。
//!
//! 两类能力（对应需求里的两种语义）：
//!
//! 1. **截取当前全屏（Screenshot 接口）** —— [`PortalCapturer`] / [`capture_screenshot`]。
//!    - 跨 compositor：KDE/GNOME/wlroots 都实现了 portal 的 Screenshot，写一次通用。
//!    - 做法：`interactive=false` 让 portal 直接截一张当前全屏（合成后、受遮挡影响），
//!      读回文件转 `RgbaImage`。**不是录屏**，就是「截一张图」。
//!    - 用途：判断当前屏幕上有什么、自动定位某个 app 窗口、闭环验证点击结果等。
//!    - 优点：轻量、非交互不弹窗、无需 PipeWire 建流。
//!
//! 2. **录屏 + 抽帧（ScreenCast 接口）** —— [`screencast::ScreenCastCapturer`]。
//!    - 走 PipeWire 消费流，可选 Monitor（全屏）或 Window（某窗口本体，不受遮挡）。
//!    - 用途：持续录屏、按需抽帧、抓「窗口自身合成流」（遮挡无关）以反推窗口偏移。
//!
//! 预留后端（TODO，按环境接入）：
//! - wlroots（Sway/Hyprland）：`zwlr_screencopy_manager_v1`（smithay-client-toolkit）。
//! - X11：x11rb 的 GetImage。
//! - Android(waydroid)：waydroid 自带截图。

use anyhow::{Context, Result};
use image::{RgbaImage, imageops};

/// 基于 xdg-desktop-portal **ScreenCast** + PipeWire 的后端（全屏 / 选窗口，窗口流不受遮挡）。
pub mod screencast;
pub use screencast::ScreenCastCapturer;

/// 基于 xdg-desktop-portal **Screenshot** 的后端（截当前全屏，非录屏）。
pub use self::PortalCapturer as ScreenshotCapturer;

/// 抓图后端统一接口。
pub trait Capturer: Sync {
    /// 抓取全屏，返回 RGBA 图。
    fn capture_fullscreen(&self) -> impl std::future::Future<Output = Result<RgbaImage>> + Send;

    /// 抓取指定区域 `(x, y, w, h)`（屏幕坐标，像素），返回裁切后的 RGBA 图。
    /// 默认实现：抓全屏后在 Rust 里裁切。
    fn capture_region(
        &self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> impl std::future::Future<Output = Result<RgbaImage>> + Send {
        async move {
            let full = self.capture_fullscreen().await?;
            Ok(crop_region(&full, x, y, w, h))
        }
    }
}

/// 把全屏图裁切到指定区域（越界部分按实际尺寸 clamp）。
pub fn crop_region(img: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> RgbaImage {
    let (iw, ih) = (img.width(), img.height());
    let x = x.min(iw.saturating_sub(1));
    let y = y.min(ih.saturating_sub(1));
    let w = w.min(iw - x);
    let h = h.min(ih - y);
    if w == 0 || h == 0 {
        return img.clone();
    }
    imageops::crop_imm(img, x, y, w, h).to_image()
}

/// 从文件加载为 RGBA 图。
pub fn load_rgba(path: &str) -> Result<RgbaImage> {
    let dyn_img = image::open(path).with_context(|| format!("读取图片失败: {}", path))?;
    Ok(dyn_img.to_rgba8())
}

/// 截取**当前屏幕全屏**一张（xdg-desktop-portal Screenshot 接口，`interactive=false`）。
///
/// 这是「截一张图」而非「录屏」：portal 直接合成当前全屏返回，受遮挡影响（符合
/// 全屏语义），无需 PipeWire 建流，也无需选择窗口。非常适合「判断屏幕上现在有什么」
/// 「自动定位某个 app 窗口」「闭环验证点击结果」等场景。
///
/// 等价于 `PortalCapturer::new().capture_fullscreen().await`，但无需先构造类型。
///
/// 注意：首次在桌面环境里调用可能弹出一次授权对话框（是否允许截图），授权后
/// 通常可被 session 记住，后续不再弹窗。
pub async fn capture_screenshot() -> Result<RgbaImage> {
    capture_via_portal().await
}

/// 基于 xdg-desktop-portal Screenshot 的后端（跨 compositor）。
pub struct PortalCapturer {
    /// 可选的应用/窗口标识，部分 compositor 用它限定抓图范围。
    _app_id: Option<String>,
}

impl PortalCapturer {
    pub fn new() -> Self {
        Self { _app_id: None }
    }

    pub fn with_app_id(app_id: impl Into<String>) -> Self {
        Self {
            _app_id: Some(app_id.into()),
        }
    }
}

impl Default for PortalCapturer {
    fn default() -> Self {
        Self::new()
    }
}

impl Capturer for PortalCapturer {
    async fn capture_fullscreen(&self) -> Result<RgbaImage> {
        capture_via_portal().await
    }
}

/// 通过 xdg-desktop-portal 的 Screenshot 接口抓全屏，读回文件转成 RgbaImage。
async fn capture_via_portal() -> Result<RgbaImage> {
    use ashpd::desktop::screenshot::Screenshot;

    // 走 builder：非交互截图，send 得到 Request<Screenshot>，response() 取结果。
    let request = Screenshot::request().interactive(false).send().await?;
    let response = request
        .response()
        .context("等待截图完成失败（可能需要在桌面环境中授权）")?;

    let uri = response.uri().to_string();
    let path = uri.strip_prefix("file://").unwrap_or(&uri).to_string();
    let path = if let Ok(decoded) = url_decode(&path) {
        decoded
    } else {
        path
    };

    image::open(&path)
        .with_context(|| format!("读取截图文件失败: {}", path))
        .map(|img| img.to_rgba8())
}

/// 极简 URL decode：把 %XX 还原成字节，再按 UTF-8 解释（xdg 截图路径可能含
/// 非 ASCII，如中文「图片」目录，必须正确解码，否则路径错乱找不到文件）。
fn url_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// 造一张 w×h 的纯色图，便于验证裁切尺寸与像素。
    fn solid(w: u32, h: u32, color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(color))
    }

    #[test]
    fn crop_region_normal() {
        let img = solid(100, 80, [1, 2, 3, 255]);
        let cropped = crop_region(&img, 10, 20, 30, 40);
        assert_eq!(cropped.width(), 30);
        assert_eq!(cropped.height(), 40);
        // 拷贝的像素值应与源一致。
        assert_eq!(*cropped.get_pixel(0, 0), Rgba([1, 2, 3, 255]));
    }

    #[test]
    fn crop_region_clamps_overflow() {
        // x 越界 → 夹到 iw-1；w 越界 → 夹到 iw-x。
        let img = solid(100, 80, [9, 9, 9, 255]);
        let cropped = crop_region(&img, 90, 70, 50, 50);
        assert_eq!(cropped.width(), 10); // 100 - 90
        assert_eq!(cropped.height(), 10); // 80 - 70
    }

    #[test]
    fn crop_region_zero_size_returns_clone() {
        // w=0 时按守卫返回原图克隆（尺寸不变）。
        let img = solid(40, 30, [4, 5, 6, 255]);
        let cropped = crop_region(&img, 0, 0, 0, 10);
        assert_eq!(cropped.width(), 40);
        assert_eq!(cropped.height(), 30);
    }

    #[test]
    fn url_decode_plain() {
        assert_eq!(url_decode("hello_world.png").unwrap(), "hello_world.png");
    }

    #[test]
    fn url_decode_space() {
        assert_eq!(
            url_decode("Screenshot%20Test.png").unwrap(),
            "Screenshot Test.png"
        );
    }

    #[test]
    fn url_decode_chinese_path() {
        // 中文目录「图片」在 portal 返回路径里被 percent-encode 成 UTF-8 字节。
        // 「图」= E5 9B BE，「片」= E7 89 87。
        let enc = "/home/aa/%E5%9B%BE%E7%89%87/Screenshot_20260731.png";
        assert_eq!(
            url_decode(enc).unwrap(),
            "/home/aa/图片/Screenshot_20260731.png"
        );
    }

    #[test]
    fn url_decode_mixed() {
        // 既有 ASCII 又有非 ASCII。
        let enc = "%E8%AE%A2%E5%8D%95%20A1024.png";
        assert_eq!(url_decode(enc).unwrap(), "订单 A1024.png");
    }
}
