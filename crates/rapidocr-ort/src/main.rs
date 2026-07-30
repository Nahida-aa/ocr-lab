//! 命令行：`rapidocr-ort --model v3 screenshot.png`
//!
//! 输出 JSON 数组，每个元素含 text / score / bbox / center。

use anyhow::{Context, Result};
use clap::Parser;
use ndarray::Array3;
use rapidocr_ort::{ModelProfile, OcrEngine, OcrResult};
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(name = "rapidocr-ort", about = "PP-OCR 文字识别（基于 ONNX Runtime）")]
struct Cli {
    /// 模型套件：v3 / v6-tiny / v6-medium
    #[arg(long, value_enum, default_value_t = ModelProfile::V3)]
    model: ModelProfile,

    /// 输入图片路径
    image: String,

    /// 模型目录（默认仓库根 models/rapidocr）
    #[arg(long, default_value = "models/rapidocr")]
    model_dir: String,
}

#[derive(Serialize)]
struct Output {
    model: String,
    results: Vec<OcrResult>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut engine = OcrEngine::from_profile(cli.model, std::path::Path::new(&cli.model_dir))
        .context("构建 OCR 引擎失败")?;

    // 读图 -> RGB HWC u8
    let img = image::open(&cli.image)
        .with_context(|| format!("读取图片失败: {}", cli.image))?
        .to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let data = img.into_raw();
    let arr = Array3::from_shape_vec((h, w, 3), data)
        .context("图像数据重塑失败（维度不匹配）")?;

    let results = engine.detect(&arr).context("OCR 推理失败")?;

    let out = Output {
        model: format!("{:?}", cli.model),
        results,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).context("序列化结果失败")?
    );
    Ok(())
}
