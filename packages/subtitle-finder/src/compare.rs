//! 跨帧比较：判断两帧字幕内容是否变化（CompareTwoSubs / CompareTwoSubsOptimal）。
//!
//! 复刻 VideoSubFinder：两帧（与各自 ILA 交集后）逐像素比较差异比例，用
//! `veple`/`ilaple` 阈值判断字幕是否变化。

use ndarray::Array3;

/// 比较两帧字幕是否不同。返回 true 表示「字幕内容变化了」。
pub fn subs_differ(_a: &Array3<u8>, _b: &Array3<u8>, _params: &super::params::Params) -> bool {
    todo!("复刻 CompareTwoSubs / CompareTwoSubsOptimal")
}
