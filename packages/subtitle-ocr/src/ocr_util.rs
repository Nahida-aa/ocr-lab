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
    let text: Vec<&str> = boxes.iter().map(|l| l.text.as_str()).collect();
    let text = text.join(" ");
    let confidence = if boxes.is_empty() {
        0.0
    } else {
        boxes.iter().map(|l| l.text_confidence as f64).sum::<f64>() / boxes.len() as f64
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
                .flat_map(|l| l.box_.iter().copied())
                .map(glam::Vec2::from_array),
        )
    };
    FrameResult {
        text,
        confidence,
        boxes: boxes.to_vec(),
        x_range,
        y_range,
        timestamp_ms: 0,
    }
}
