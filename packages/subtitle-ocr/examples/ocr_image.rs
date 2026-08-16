//! 单图字幕 OCR 调试工具（保留）：用 `SubtitleOcr`（对齐 cpp 的 bottom_only /
//! subtitle_only / NMS 流程）对单张图跑字幕 OCR，打印每个识别框的全部字段。
//!
//! 用法：`cargo run -p subtitle-ocr --example ocr_image -- <image.png> [--subtitle-only] [--no-bottom-only] [--warp]`
//!   - 默认 `bottom_only=true`（只裁底部 40% 送 OCR，对齐 cpp）。
//!   - `--subtitle-only`：再按 y 中心落在底部比例区间过滤。
//!   - `--no-bottom-only`：关闭 bottom_only（全图 OCR）。
//!   - `--warp`：用透视矫正裁剪（warpPerspective）替代轴对齐包围盒。
//!
//! 输出字段：text / text_confidence(tc) / box_confidence(bc) / bbox / center /
//! x_range / y_range，便于排查识别结果与坐标。
use std::path::Path;

use rapidocr_ort::ModelProfile;
use subtitle_ocr::{OcrOptions, SubtitleOcr};

/// 仓库根：`packages/subtitle-ocr` 的上两级。
fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("parent")
        .parent()
        .expect("parent")
        .to_path_buf()
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: ocr_image <image.png> [--subtitle-only] [--no-bottom-only] [--warp]");
        std::process::exit(1);
    }
    let img_path = &args[1];
    let subtitle_only = args.iter().any(|a| a == "--subtitle-only");
    let bottom_only = !args.iter().any(|a| a == "--no-bottom-only");
    let warp = args.iter().any(|a| a == "--warp");

    let opts = OcrOptions {
        bottom_only,
        subtitle_only,
        use_warp_crop: warp,
        ..Default::default()
    };

    let root = repo_root();
    let model_dir = root.join("models/rapidocr");
    let img = rapidocr_ort::load_image(Path::new(img_path))?;
    let mut ocr = SubtitleOcr::from_profile(ModelProfile::V4, &model_dir, opts)?;
    let results = ocr.ocr_image(&img)?;

    println!(
        "图像: {}  (bottom_only={}, subtitle_only={}, warp={})",
        img_path, bottom_only, subtitle_only, warp
    );
    if results.is_empty() {
        println!("(无文字)");
    } else {
        for r in &results {
            println!(
                "text={:?} tc={:.4} bc={:.4}\n  bbox={:?}\n  x_range={:?} y_range={:?} center={:?}",
                r.text,
                r.text_confidence,
                r.box_confidence,
                r.bbox,
                r.x_range,
                r.y_range,
                r.center,
            );
        }
    }
    Ok(())
}
