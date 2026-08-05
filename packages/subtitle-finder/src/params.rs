//! 筛选参数：对齐 VideoSubFinder 的全局参数默认值（SSAlgorithms.cpp）。

/// 筛选参数。
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// 字幕帧长度 / 滑动窗口（`g_DL`）。
    pub dl: usize,
    /// 水平条带高度（`g_segh`）。
    pub segh: usize,
    /// 文字占比阈值（`g_tp`）。
    pub tp: f32,
    /// 最小文字长度（百分比，`g_mtpl`）。
    pub mtpl: f32,
    /// 跨帧文字差异阈值（`g_veple`）。
    pub veple: f32,
    /// ILA 差异阈值（`g_ilaple`）。
    pub ilaple: f32,
    /// 字幕最短持续（帧数，`g_max_dl_down`）。
    pub max_dl_down: usize,
    /// 字幕最长持续（帧数，`g_max_dl_up`）。
    pub max_dl_up: usize,
    /// ROI 字幕区 y 范围（`ymin`/`ymax`），None 则全图。
    pub roi_y: Option<(usize, usize)>,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            dl: 6,
            segh: 3,
            tp: 0.3,
            mtpl: 0.022,
            veple: 0.30,
            ilaple: 0.30,
            max_dl_down: 20,
            max_dl_up: 40,
            roi_y: None,
        }
    }
}
