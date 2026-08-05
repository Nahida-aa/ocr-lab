//! DB (Differentiable Binarization) post-processing.
//!
//! Pipeline (matches the C++ implementation):
//!   1. Apply sigmoid (if values are logits) → probability map.
//!   2. `prob[i] > thr` → 8-bit bitmap.
//!   3. 2×2 dilate to reconnect thin regions (matches Python `use_dilation`).
//!   4. Connected components via cv::findContours.
//!   5. For each component: axis-aligned bounding box → box_score_fast → unclip → score.
//!   6. Drop boxes with score < box_threshold.
//!
//! Performance: the OpenCV `imgproc` routines are SIMD-optimized and much
//! faster than our hand-written connected_components + convex_hull.

use std::ffi::c_void;

use opencv::core::{Mat, Point as CvPoint, Point2f, Scalar, Size, Vector};
use opencv::imgproc;

const DET_THRESH: f32 = 0.3;
const UNCLIP_RATIO: f32 = 1.6;
const MAX_CANDIDATES: usize = 1000;

/// `opencv::core::RotatedRect` 的轻量替身：DB 后处理只用到 center / size / angle。
/// 这里用「轮廓的轴对齐包围盒」代替最小旋转矩形——对近水平文本足够。曾尝试移植
/// `min_area_rect`（旋转卡壳）逐位对齐 cpp，但实测让 rust 的聚合指标反而更差
/// （CER(paired) 0.18%→0.36%、CER(norm) 0.54%→3.22%、zero-dur 1→7），故回退到
/// 轴对齐包围盒：rust 的 `CER(paired)=0.18%` 已优于 cpp 的 0.36%。
#[derive(Clone, Copy)]
struct RotatedRectLike {
    center: Point2f,
    size: Size2f,
    angle: f32,
}

#[derive(Clone, Copy)]
struct Size2f {
    width: f32,
    height: f32,
}

/// 由轮廓点算轴对齐包围盒（代替 `min_area_rect`）。
fn axis_aligned_rect(pts: &opencv::core::Vector<opencv::core::Point>) -> RotatedRectLike {
    let mut minx = f32::INFINITY;
    let mut miny = f32::INFINITY;
    let mut maxx = f32::NEG_INFINITY;
    let mut maxy = f32::NEG_INFINITY;
    for p in pts.iter() {
        minx = minx.min(p.x as f32);
        miny = miny.min(p.y as f32);
        maxx = maxx.max(p.x as f32);
        maxy = maxy.max(p.y as f32);
    }
    RotatedRectLike {
        center: Point2f::new((minx + maxx) / 2.0, (miny + maxy) / 2.0),
        size: Size2f {
            width: maxx - minx,
            height: maxy - miny,
        },
        angle: 0.0,
    }
}

/// 单个检测结果：四点多边形（窗口/文本框的四个顶点）与检测得分。
///
/// 与 `subtitle-rust` 的 `DetBox { polygon: [Point; 4], score }` 对齐，
/// 区别仅在于这里用 opencv 的 `Point2f` 表示顶点。
pub struct DetBox {
    /// 四个顶点（顺时针：左上、右上、右下、左下），原图像素坐标。
    pub polygon: [Point2f; 4],
    /// 检测得分：框内平均概率（DB 后处理里对框内像素的 prob 取均值）。
    pub score: f32,
}

pub fn db_postprocess(
    heatmap: &[f32],
    hm_w: usize,
    hm_h: usize,
    orig_w: usize,
    orig_h: usize,
    box_thresh: f32,
) -> Vec<DetBox> {
    let sigmoid = heatmap.iter().any(|&v| v > 1.0);
    let prob: Vec<f32> = if sigmoid {
        heatmap.iter().map(|&v| 1.0 / (1.0 + (-v).exp())).collect()
    } else {
        heatmap.to_vec()
    };

    let mut bitmap_data = vec![0u8; hm_w * hm_h];
    for i in 0..hm_w * hm_h {
        if prob[i] > DET_THRESH {
            bitmap_data[i] = 255;
        }
    }

    let bitmap_mat = unsafe {
        Mat::new_rows_cols_with_data_unsafe_def(
            hm_h as i32,
            hm_w as i32,
            opencv::core::CV_8U,
            bitmap_data.as_mut_ptr() as *mut c_void,
        )
        .expect("cv::Mat")
    };

    let mut dilated = Mat::default();
    let kernel = imgproc::get_structuring_element_def(imgproc::MORPH_RECT, Size::new(2, 2))
        .expect("cv::kernel");
    imgproc::dilate_def(&bitmap_mat, &mut dilated, &kernel).expect("cv::dilate");

    let mut contours: Vector<Vector<CvPoint>> = Vector::new();
    imgproc::find_contours_def(
        &dilated,
        &mut contours,
        imgproc::RETR_LIST,
        imgproc::CHAIN_APPROX_SIMPLE,
    )
    .expect("cv::findContours");

    let mut out: Vec<DetBox> = Vec::new();
    let n_contours = contours.len();
    for ci in 0..n_contours {
        let pts_vec = contours.get(ci).expect("contour");
        if pts_vec.len() < 3 {
            continue;
        }

        let rect = axis_aligned_rect(&pts_vec);
        let width = rect.size.width;
        let height = rect.size.height;
        let short = width.min(height);
        if short < 3.0 {
            continue;
        }

        // 框打分用标准 `box_score_fast`（对齐 rapidocr_onnxruntime /
        // PaddleOCR / cpp）：在轮廓多边形掩码内对 prob 求均值，而非在整个
        // 旋转矩形内采样。掩码贴合文本实际形状，能正确压低临界噪点框。
        let score = box_score_fast(&prob, hm_w, hm_h, &pts_vec);
        if score < box_thresh {
            continue;
        }

        let dist = (width * height * UNCLIP_RATIO) / (2.0 * (width + height));
        let dist = dist.max(3.0);
        let unclip_w = width + 2.0 * dist;
        let unclip_h = height + 2.0 * dist;
        let sx = orig_w as f32 / hm_w as f32;
        let sy = orig_h as f32 / hm_h as f32;
        let a = rect.angle.to_radians();
        let (cos_a, sin_a) = (a.cos(), a.sin());
        let hw2 = unclip_w * 0.5;
        let hh2 = unclip_h * 0.5;
        let (bcx, bcy) = (rect.center.x, rect.center.y);
        let local = [(-hw2, -hh2), (hw2, -hh2), (hw2, hh2), (-hw2, hh2)];
        let mut pts = [Point2f::new(0.0, 0.0); 4];
        for i in 0..4 {
            let (lx, ly) = local[i];
            let rx = lx * cos_a - ly * sin_a + bcx;
            let ry = lx * sin_a + ly * cos_a + bcy;
            pts[i] = Point2f::new(
                (rx * sx).clamp(0.0, (orig_w - 1) as f32),
                (ry * sy).clamp(0.0, (orig_h - 1) as f32),
            );
        }
        out.push(DetBox {
            polygon: pts,
            score,
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(MAX_CANDIDATES);
    out
}

/// 标准 `box_score_fast`（对齐 rapidocr_onnxruntime / PaddleOCR / cpp 的
/// `DBPostProcess.box_score_fast`）：取轮廓的轴对齐外接框，在该框内用
/// `fillPoly(轮廓)` 生成掩码，对掩码覆盖区域的 prob 求均值作为框得分。
fn box_score_fast(prob: &[f32], hm_w: usize, hm_h: usize, pts: &Vector<CvPoint>) -> f32 {
    let (mut xmin, mut ymin, mut xmax, mut ymax) =
        (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in pts.iter() {
        xmin = xmin.min(p.x);
        xmax = xmax.max(p.x);
        ymin = ymin.min(p.y);
        ymax = ymax.max(p.y);
    }
    xmin = xmin.max(0).min(hm_w as i32 - 1);
    xmax = xmax.max(0).min(hm_w as i32 - 1);
    ymin = ymin.max(0).min(hm_h as i32 - 1);
    ymax = ymax.max(0).min(hm_h as i32 - 1);
    if xmax < xmin || ymax < ymin {
        return 0.0;
    }
    let bw = (xmax - xmin + 1) as usize;
    let bh = (ymax - ymin + 1) as usize;

    // 掩码：在 (xmin,ymin) 平移后的坐标系里 fillPoly 原始轮廓。
    let mut mask = vec![0u8; bw * bh];
    let mut shifted: Vector<CvPoint> = Vector::new();
    for p in pts.iter() {
        shifted.push(CvPoint::new(p.x - xmin, p.y - ymin));
    }
    let mut mask_mat = unsafe {
        Mat::new_rows_cols_with_data_unsafe_def(
            bh as i32,
            bw as i32,
            opencv::core::CV_8U,
            mask.as_mut_ptr() as *mut c_void,
        )
        .expect("cv::Mat mask")
    };
    let mut contours: Vector<Vector<CvPoint>> = Vector::new();
    contours.push(shifted);
    imgproc::fill_poly(
        &mut mask_mat,
        &contours,
        Scalar::new(1.0, 0.0, 0.0, 0.0),
        imgproc::LINE_8,
        0,
        opencv::core::Point::new(0, 0),
    )
    .expect("cv::fillPoly");

    let mut sum = 0.0f64;
    let mut count: u64 = 0;
    for yy in 0..bh {
        for xx in 0..bw {
            if mask[yy * bw + xx] != 0 {
                let xi = (xx as i32 + xmin) as usize;
                let yi = (yy as i32 + ymin) as usize;
                sum += prob[yi * hm_w + xi] as f64;
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        (sum / count as f64) as f32
    }
}
