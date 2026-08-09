//! 字幕框的纵向（y）统计：由多帧识别结果估算字幕带的位置 / 高度分布。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `computeBoxYStats`：输入
//! [`FrameResult`] 序列，展平各帧非空文本的框，统计 y 值域的均值 / 众数 / 中位数
//! 以及行高分布，供后续行对齐、离群框剔除、段置信度调整使用。

use crate::FrameResult;
use serde::Serialize;

/// 字幕框纵向统计结果（对齐 LocalDub `YStats`）。
///
/// 坐标为原图像素坐标（f32，未取整——ROI 还原后可能为小数）。
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct YStats {
    /// y 值域均值 `[top, bottom]`（所有框 top 均值、bottom 均值）
    pub avg: [f32; 2],
    /// y 值域众数 `[top, bottom]`出现最频繁的 (top, bottom) 对
    pub mode: [f32; 2],
    /// y 值域中位数 `[top, bottom]`所有 top 排序取中位、所有 bottom 排序取中位
    pub median: [f32; 2],
    /// 平均行高（所有框 `y_range[1]-y_range[0]` 的均值）。
    pub avg_height: f32,
    /// 行高中位数。
    pub median_height: f32,
    /// 行高众数（出现最频繁的行高）。
    pub mode_height: f32,
}

/// 对一组帧统计字幕框的纵向分布（位置 + 高度）。
///
/// 仅统计有文本（`text` 非空白）的框；无此类框时返回全零 [`YStats`]。
///
/// 实现对齐 LocalDub `computeBoxYStats`：均值用全体 top/bottom 平均；中位数对 top 序列
/// 与 bottom 序列分别取中位；众数为出现最频繁的取值（高度取最频繁行高，位置取最频繁
/// 的 `(top, bottom)` 对）。坐标保持 f32，不做 `Math.round` 取整。
pub fn compute_box_y_stats(frames: &[FrameResult]) -> YStats {
    // 展平各帧、过滤掉无文本行。
    let boxes: Vec<&crate::OcrBoxResult> = frames
        .iter()
        .flat_map(|f| f.boxes.iter())
        .filter(|l| !l.text.trim().is_empty())
        .collect();
    if boxes.is_empty() {
        return YStats::default();
    }

    let n = boxes.len();
    let box_ys: Vec<[f32; 2]> = boxes.iter().map(|l| l.y_range).collect();

    let sum_top: f32 = box_ys.iter().map(|[t, _]| *t).sum();
    let sum_btm: f32 = box_ys.iter().map(|[_, b]| *b).sum();
    let avg = [sum_top / n as f32, sum_btm / n as f32];

    let sum_h: f32 = boxes.iter().map(|l| l.y_range[1] - l.y_range[0]).sum();
    let avg_height = sum_h / n as f32;

    // 行高排序取中位。
    let mut heights: Vec<f32> = boxes.iter().map(|l| l.y_range[1] - l.y_range[0]).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_height = median_of(&heights);

    // 位置中位数：top 序列与 bottom 序列分别取中位。
    let mut tops: Vec<f32> = box_ys.iter().map(|[t, _]| *t).collect();
    let mut btms: Vec<f32> = box_ys.iter().map(|[_, b]| *b).collect();
    tops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    btms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = [median_of(&tops), median_of(&btms)];

    // 行高众数：出现最频繁的行高（首遇最大计数者）。
    let mut height_counts: std::collections::HashMap<i32, u32> = std::collections::HashMap::new();
    let mut mode_height_count = 0u32;
    let mut mode_height = heights[0];
    for &h in &heights {
        // 量化到整数像素再计数（与 TS 的 `Map<number>` key 行为一致）。
        let key = h.round() as i32;
        let c = height_counts.entry(key).or_insert(0);
        *c += 1;
        if *c > mode_height_count {
            mode_height_count = *c;
            mode_height = h;
        }
    }

    // 位置众数：出现最频繁的 (top, bottom) 对（用 `t,b` 字符串 key，对齐 TS）。
    let mut counts: std::collections::HashMap<(i32, i32), u32> = std::collections::HashMap::new();
    let mut max_count = 0u32;
    let mut mode = box_ys[0];
    for &[t, b] in &box_ys {
        let key = (t.round() as i32, b.round() as i32);
        let c = counts.entry(key).or_insert(0);
        *c += 1;
        if *c > max_count {
            max_count = *c;
            mode = [t, b];
        }
    }

    YStats {
        avg,
        mode,
        median,
        avg_height,
        median_height,
        mode_height,
    }
}

/// 对升序切片取中位数（偶数长取中间两数均值）。
fn median_of(arr: &[f32]) -> f32 {
    let m = arr.len() / 2;
    if arr.len() % 2 == 0 {
        (arr[m - 1] + arr[m]) / 2.0
    } else {
        arr[m]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OcrBoxResult;

    /// 构造一个框：text + y 值域，其余字段占位。
    fn box_with(text: &str, y_range: [f32; 2]) -> OcrBoxResult {
        OcrBoxResult {
            text: text.into(),
            text_confidence: 0.9,
            box_confidence: 0.9,
            box_: [
                [0.0, y_range[0]],
                [10.0, y_range[0]],
                [10.0, y_range[1]],
                [0.0, y_range[1]],
            ],
            x_range: [0.0, 10.0],
            y_range,
            center: [5.0, (y_range[0] + y_range[1]) / 2.0],
        }
    }

    fn frame(boxes: Vec<OcrBoxResult>) -> FrameResult {
        crate::FrameResult {
            text: String::new(),
            text_confidence: 0.0,
            boxes,
            x_range: [0.0, 0.0],
            y_range: [0.0, 0.0],
            timestamp_ms: 0,
        }
    }

    #[test]
    fn empty_frames_returns_zero() {
        let r = compute_box_y_stats(&[]);
        assert_eq!(r.avg, [0.0, 0.0]);
        assert_eq!(r.mode, [0.0, 0.0]);
        assert_eq!(r.median, [0.0, 0.0]);
        assert_eq!(r.avg_height, 0.0);
        assert_eq!(r.median_height, 0.0);
        assert_eq!(r.mode_height, 0.0);
    }

    #[test]
    fn skips_empty_text_boxes() {
        let f = frame(vec![
            box_with("", [10.0, 20.0]), // 空文本，应被跳过
            box_with("a", [40.0, 50.0]),
        ]);
        let r = compute_box_y_stats(&[f]);
        // 仅一个有效框：avg/median/mode 均为 [40,50]，height=10。
        assert_eq!(r.avg, [40.0, 50.0]);
        assert_eq!(r.median, [40.0, 50.0]);
        assert_eq!(r.mode, [40.0, 50.0]);
        assert_eq!(r.avg_height, 10.0);
        assert_eq!(r.median_height, 10.0);
        assert_eq!(r.mode_height, 10.0);
    }

    #[test]
    fn median_and_mode() {
        // 三个框：高度 10/20/10（众数 10）；位置各异。
        let f = frame(vec![
            box_with("a", [10.0, 20.0]),   // h=10
            box_with("b", [40.0, 60.0]),   // h=20
            box_with("c", [100.0, 110.0]), // h=10
        ]);
        let r = compute_box_y_stats(&[f]);
        assert_eq!(r.median_height, 10.0); // 排序 [10,10,20] 中位 10
        assert_eq!(r.mode_height, 10.0); // 10 出现 2 次
        // 中位数位置：tops [10,40,100] -> 40；btms [20,60,110] -> 60
        assert_eq!(r.median, [40.0, 60.0]);
    }
}
