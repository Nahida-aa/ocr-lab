use rapidocr_ort::OcrBoxResult;

use crate::FrameResult;

/// 把一图识别出的多框聚合成单图结果（纯感知后处理，`timestamp_ms` 置 0）。
///
/// 过滤 / 坐标还原 / NMS / 排序已在 [`crate::SubtitleOcr::ocr_image`] 完成，这里只做
/// 「多行拼接成一条文本 + 各框置信度取均值 + 算几何值域」。
///
/// `timestamp_ms` 故意不在此处填入（置 0），原因：同一张图可能对应多个时刻
/// （文件名 `ms_ms` 时间区间图片，start/end 两个时刻、内容相同）。保持单参数、
/// 不接 timestamp，调用方就能**先聚合一次**得到无时间的 [`FrameResult`]，再按
/// `entry.times` 展开——对每个时刻 `clone` 后仅改 `timestamp_ms`，避免对同一
/// boxes 重复聚合。携带时间的场景由调用方在 [`FrameResult`] 上赋值（如按文件名
/// `ms`/`ms_ms` 解析、或帧序号 / 视频 PTS）。
pub fn aggregate_boxes(boxes: &[OcrBoxResult]) -> FrameResult {
    // 任意两个框的 y 值域重叠 ⇒ 视为同一行，用空格拼接；否则用换行分隔多行。
    // 对齐 LocalDub utils.ts 的 sameLine 语义（boxes 已按 y 中心升序，故顺序即从上到下）。
    let same_line = boxes.len() >= 2
        && (0..boxes.len() - 1).any(|a| {
            (a + 1..boxes.len()).any(|b| {
                boxes[a].y_range[1] >= boxes[b].y_range[0]
                    && boxes[b].y_range[1] >= boxes[a].y_range[0]
            })
        });
    let sep = if same_line { " " } else { "\n" };
    let text: Vec<&str> = boxes.iter().map(|l| l.text.as_str()).collect();
    let text = text.join(sep);
    let text_confidence = if boxes.is_empty() {
        0.0
    } else {
        boxes.iter().map(|i| i.text_confidence as f64).sum::<f64>() / boxes.len() as f64
    };
    // 聚合所有行的四点坐标，取 x / y 值域（无字幕 → [0,0]）。
    // 复用 rapidocr-ort 的 points_range：把所有点展平成 Vec2 流，单遍 SSE fold 同时
    // 算出 x/y 两路值域，与 polygon_metrics 同源、无 mut。
    let (x_range, y_range) = if boxes.is_empty() {
        ([0.0, 0.0], [0.0, 0.0])
    } else {
        rapidocr_ort::points_range(
            boxes
                .iter()
                .flat_map(|i| i.box_.iter().copied())
                .map(glam::Vec2::from_array),
        )
    };
    FrameResult {
        text,
        text_confidence,
        boxes: boxes.to_vec(),
        x_range,
        y_range,
        timestamp_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidocr_ort::OcrBoxResult;

    /// 构造一个框：text + y 值域（其它字段占位）。
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

    #[test]
    fn single_line_uses_space() {
        // 两框 y 重叠 ⇒ 同行，空格拼接。
        let boxes = [
            box_with("hello", [10.0, 20.0]),
            box_with("world", [12.0, 22.0]),
        ];
        let r = aggregate_boxes(&boxes);
        assert_eq!(r.text, "hello world");
    }

    #[test]
    fn multi_line_uses_newline() {
        // 两框 y 不重叠 ⇒ 多行，换行分隔。
        let boxes = [
            box_with("line1", [10.0, 20.0]),
            box_with("line2", [40.0, 50.0]),
        ];
        let r = aggregate_boxes(&boxes);
        assert_eq!(r.text, "line1\nline2");
    }

    #[test]
    fn empty_boxes_text_is_empty() {
        let r = aggregate_boxes(&[]);
        assert_eq!(r.text, "");
        assert_eq!(r.text_confidence, 0.0);
    }
}
