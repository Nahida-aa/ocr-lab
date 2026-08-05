//! 吃视频 → 输出字幕关键帧图 + 时间轴（复刻 VideoSubFinder 的筛选管线）。
//!
//! 本包只做「找到字幕变化的关键帧 + 时间轴」，不做 OCR 识别（那是
//! `rapidocr-ort` 下游的事），对应官方 `VideoSubFinder → RapidVideOCR` 分工。
//!
//! 设计见 `DESIGN.md`。核心是 `FastSearchSubtitles` 状态机（DL=6 滑动窗口 +
//! 相邻帧交集去噪 + 水平投影检测文字 + 跨帧比较判断字幕变化），精确复刻
//! VideoSubFinder 的 30fps 逐帧时间行为，避免抽样帧的时间偏移。

pub mod compare;
pub mod filter;
pub mod frame;
pub mod params;
pub mod preprocess;
pub mod state;

use anyhow::Result;

/// 一个字幕关键帧：一段字幕的一张代表帧 + 时间轴。
#[derive(Debug, Clone)]
pub struct Keyframe {
    /// 字幕段起始时间（毫秒）。
    pub start_ms: u64,
    /// 字幕段结束时间（毫秒）。
    pub end_ms: u64,
    /// 代表帧的 BGR 图像（H×W×3，0-255）。
    pub frame: ndarray::Array3<u8>,
}

/// 对视频逐帧筛选，输出字幕关键帧 + 时间轴。
///
/// `video` 为视频路径；`params` 为筛选参数（对齐 VideoSubFinder 默认值，
/// 见 `params::Params::default()`）。
///
/// TODO(state)：逐帧解码已接入，但筛选状态机（FastSearchSubtitles）未实现，
/// 当前仅解码并统计帧数。
pub fn find_keyframes(video: &std::path::Path, _params: &params::Params) -> Result<Vec<Keyframe>> {
    let mut count = 0usize;
    let mut first_frame: Option<ndarray::Array3<u8>> = None;
    frame::for_each_frame(video, |arr| {
        count += 1;
        if first_frame.is_none() {
            first_frame = Some(arr);
        }
        Ok(true)
    })?;
    eprintln!("subtitle-finder: 解码 {} 帧", count);
    anyhow::bail!(
        "subtitle-finder 筛选状态机未实现（DESIGN.md），已解码 {} 帧（首帧 {}x{}）",
        count,
        first_frame.as_ref().map(|f| f.dim().1).unwrap_or(0),
        first_frame.as_ref().map(|f| f.dim().0).unwrap_or(0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_defaults_match_videosubfinder() {
        let p = params::Params::default();
        assert_eq!(p.dl, 6);
        assert_eq!(p.segh, 3);
        assert!((p.tp - 0.3).abs() < 1e-6);
        assert!((p.mtpl - 0.022).abs() < 1e-6);
        assert!((p.veple - 0.30).abs() < 1e-6);
        assert!((p.ilaple - 0.30).abs() < 1e-6);
        assert_eq!(p.max_dl_down, 20);
        assert_eq!(p.max_dl_up, 40);
    }

    /// 集成测试：用 bench 的真实视频解码前几帧，验证 30fps 解码管线 + 分辨率正确。
    /// 依赖 tests/bench/subtitle-ocr/ref/video_source.mp4 存在（不入库，需自备）。
    #[test]
    #[ignore] // 依赖真实视频文件，手动跑
    fn decodes_video_frames() {
        let video = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/bench/subtitle-ocr/ref/video_source.mp4");
        assert!(video.exists(), "缺少测试视频: {}", video.display());

        let mut count = 0usize;
        let mut dims = None;
        frame::for_each_frame(&video, |arr| {
            count += 1;
            if dims.is_none() {
                let (h, w, _) = arr.dim();
                dims = Some((h, w));
            }
            // 只解前 30 帧验证管线，避免全量慢。
            Ok(count < 30)
        })
        .expect("解码失败");

        assert_eq!(dims, Some((720, 1280)), "720p 分辨率");
        assert_eq!(count, 30);
        eprintln!("subtitle-finder 解码前 {} 帧，{}x{}", count, dims.unwrap().0, dims.unwrap().1);
    }
}
