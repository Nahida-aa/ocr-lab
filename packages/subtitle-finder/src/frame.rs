//! 逐帧解码（视频 → 帧流），复刻 VideoSubFinder 的 30fps 逐帧。
//!
//! 用 `opencv::videoio::VideoCapture` 解码视频（与 C++ `VideoSubFinder` 的 OpenCV
//! 后端一致），逐帧产出 BGR `Array3<u8>`（H×W×3），供预处理（交集/投影）使用。
//!
//! ⚠️ 之前用 `ffmpeg-next` 解码，其 swscale 默认输出 **bt601** BGR，而 C++/OpenCV
//! 输出 **bt709** BGR（差 ±1-8）。该输入色彩差异让 `get_im_ff` 阈值边缘像素判定不同
//! → 幽灵带 → 字幕段丢失/过度切分。改用 OpenCV VideoCapture 后输入与 C++ 完全一致。
//! （配合 `imgops::bgr_to_yuv` 用 `cvtColor`，彻底对齐。）
//!
//! 提供两种用法：
//! - [`FrameStepper`]：**流式逐帧**（`next()` 按需拉帧，可暂停/续解）。
//! - [`for_each_frame`]：一次性全量回调解码（基于 `FrameStepper`，保持兼容）。

use anyhow::{Context, Result};
use ndarray::Array3;

/// 流式逐帧解码器：持有 OpenCV `VideoCapture`，`next()` 按需拉一帧。
pub struct FrameStepper {
    cap: opencv::videoio::VideoCapture,
    total_duration_ms: i64,
    total_frames: i64,
    fps: f64,
    decoded_count: i64, // 已产出帧数（用于无 PTS 兜底估算）
}

impl FrameStepper {
    /// 打开视频，准备流式解码。
    pub fn open(video: &std::path::Path) -> Result<Self> {
        use opencv::prelude::*;
        let path = video.to_string_lossy().to_string();
        let mut cap = opencv::videoio::VideoCapture::from_file(&path, opencv::videoio::CAP_ANY)
            .context("OpenCV 打开视频失败")?;
        if !cap.is_opened().context("is_opened 失败")? {
            anyhow::bail!("视频无法打开");
        }

        let fps = cap
            .get(opencv::videoio::CAP_PROP_FPS)
            .context("取 FPS 失败")?;
        let total_frames = cap
            .get(opencv::videoio::CAP_PROP_FRAME_COUNT)
            .context("取帧数失败")? as i64;
        let total_duration_ms = if fps > 0.0 {
            (total_frames as f64 / fps * 1000.0).round() as i64
        } else {
            0
        };

        Ok(Self {
            cap,
            total_duration_ms,
            total_frames,
            fps,
            decoded_count: 0,
        })
    }

    /// 视频总时长（毫秒）。
    pub fn total_duration_ms(&self) -> i64 {
        self.total_duration_ms
    }

    /// 视频总帧数。0 表示未知。
    pub fn total_frames(&self) -> i64 {
        self.total_frames
    }

    /// 拉下一帧（BGR `Array3`），EOF 返回 `None`。
    ///
    /// 返回 `(帧像素, pts_ms)`：`pts_ms` 为当前帧时间戳（毫秒），用
    /// `CAP_PROP_POS_MSEC`（OpenCV 后端即帧的真实呈现时间）。POS_MSEC 不可靠时
    /// 兜底用帧号 × 1000/fps。
    pub fn next(&mut self) -> Result<Option<(Array3<u8>, i64)>> {
        use opencv::prelude::*;
        let mut mat = opencv::core::Mat::default();
        if !self.cap.read(&mut mat).context("read 失败")? {
            return Ok(None); // EOF
        }
        let h = mat.rows() as usize;
        let w = mat.cols() as usize;
        let channels = mat.channels() as usize;
        if channels != 3 {
            anyhow::bail!("OpenCV 帧不是 3 通道 BGR（channels={}）", channels);
        }
        // OpenCV read() 输出连续 BGR，直接转 Array3。
        let data = mat.data_bytes().context("取 Mat 数据失败")?;
        let arr = Array3::from_shape_vec((h, w, 3), data.to_vec()).context("Array3 形状错误")?;

        // PTS：优先 POS_MSEC（与 C++/OpenCV 语义一致），不可靠则用帧号估算。
        let pos_msec = self
            .cap
            .get(opencv::videoio::CAP_PROP_POS_MSEC)
            .context("取 POS_MSEC 失败")? as i64;
        let pts_ms = if pos_msec > 0 {
            pos_msec
        } else if self.fps > 0.0 {
            (self.decoded_count as f64 * 1000.0 / self.fps).round() as i64
        } else {
            self.decoded_count * 1000 / 30
        };
        self.decoded_count += 1;
        Ok(Some((arr, pts_ms)))
    }
}

/// 一次性全量回调解码（基于 `FrameStepper`）。`f` 返回 `false` 可提前停止。
///
/// `f` 收到 `(BGR Array3<u8>, pts_ms)`：帧像素（H×W×3，0-255）与该帧真实
/// 呈现时间戳（毫秒）。
pub fn for_each_frame(
    video: &std::path::Path,
    mut f: impl FnMut(Array3<u8>, i64) -> Result<bool>,
) -> Result<()> {
    let mut stepper = FrameStepper::open(video)?;
    while let Some((arr, pts_ms)) = stepper.next()? {
        if !f(arr, pts_ms)? {
            break;
        }
    }
    Ok(())
}
