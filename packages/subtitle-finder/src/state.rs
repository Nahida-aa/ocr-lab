//! 字幕时间轴状态机：FastSearchSubtitles。
//!
//! 复刻 VideoSubFinder 的核心状态机：用 `bf/ef`（起止帧）、`bt/et`（起止时间）、
//! `DL`、`max_dl_down/up` 跟踪字幕段，只在「字幕内容变化」时输出关键帧。
//! 这是「时间无偏移」的关键，必须精确对齐。

use super::Keyframe;

/// 字幕时间轴状态机。
///
/// TODO(state)：实现 FastSearchSubtitles 的状态迁移。
pub struct StateMachine;

impl StateMachine {
    pub fn new(_params: &super::params::Params) -> Self {
        Self
    }

    /// 喂入一帧（已判定的字幕状态），推进状态机，可能产出一个关键帧。
    /// `has_sub` 为这帧是否有字幕，`frame` 为当前帧。
    pub fn feed(&mut self, _has_sub: bool, _frame: &ndarray::Array3<u8>) -> Option<Keyframe> {
        todo!("复刻 FastSearchSubtitles 状态机")
    }
}
