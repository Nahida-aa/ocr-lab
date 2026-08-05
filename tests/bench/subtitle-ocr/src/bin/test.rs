//! subtitle-ocr 三实现（cpp / py / rust）的灰盒正确性测试。
//!
//! 用法：
//!   cargo run -p bench-subtitle-ocr --bin test [--impl cpp|py|rust]
//!
//! 行为：直接读 tests/.test-frames 下的帧，跑对应实现的 --dir 模式，
//! 校验输出 JSON 结构（file / segments / text / confidence / box）。
//! py / rust 尚未实现时优雅跳过（退出非 0）。

use std::path::{Path, PathBuf};
use std::process::Command;

const IMPLS: &[&str] = &["cpp", "py", "rust"];

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../tests/bench/subtitle-ocr
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// 帧源目录（仓库内既存的 tests/.test-frames，不再复制到包内临时目录）。
fn frames_dir() -> PathBuf {
    repo_root().join("tests").join(".test-frames")
}

fn models_dir() -> PathBuf {
    repo_root().join("models").join("rapidocr")
}

/// 统计帧目录下图片文件数。
fn count_frames() -> usize {
    let dir = frames_dir();
    assert!(dir.exists(), "帧源目录不存在: {}", dir.display());
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| {
            n.ends_with(".jpg") || n.ends_with(".jpeg") || n.ends_with(".png") || n.ends_with(".bmp")
        })
        .collect();
    names.len()
}

/// 校验 cpp --dir 输出的 JSON 结构（数组，每项含 file/segments，segment 含字段）。
fn check_cpp_output(stdout: &str) {
    let v: serde_json::Value = serde_json::from_str(stdout).expect("cpp 输出不是合法 JSON");
    let arr = v.as_array().expect("cpp 输出应为 JSON 数组");
    let n = count_frames();
    assert_eq!(arr.len(), n, "应处理 {n} 帧，实际 {}", arr.len());
    for item in arr {
        assert!(item.get("file").is_some(), "缺 file 字段");
        let segs = item.get("segments").and_then(|s| s.as_array()).expect("segments 应为数组");
        for seg in segs {
            let _text = seg.get("text").and_then(|t| t.as_str()).expect("text 应为字符串");
            let conf = seg.get("confidence").and_then(|c| c.as_f64()).expect("confidence 应为数字");
            assert!((0.0..=1.0).contains(&conf), "confidence 越界: {conf}");
            let box_ = seg.get("box").and_then(|b| b.as_array()).expect("box 应为数组");
            if !box_.is_empty() {
                assert_eq!(box_.len(), 4, "box 应为 4 点");
            }
        }
    }
}

fn run_cpp() -> bool {
    let bin = repo_root()
        .join("packages")
        .join("subtitle-ocr-cpp")
        .join("build")
        .join("subtitle_ocr_ort_cpp");
    if !bin.exists() {
        eprintln!("[cpp] 二进制不存在: {}，先构建 packages/subtitle-ocr-cpp", bin.display());
        return false;
    }
    let md = models_dir();
    let out = Command::new(&bin)
        .arg("--dir")
        .arg(frames_dir())
        .arg("0.5")
        .arg("--no-nms")
        .env("OCR_MODELS_DIR", &md)
        .env("OCR_KEYS_PATH", md.join("ppocr_keys.json"))
        .output()
        .expect("spawn cpp 失败");
    if !out.status.success() {
        eprintln!("[cpp] 退出码非零:\n{}", String::from_utf8_lossy(&out.stderr));
        return false;
    }
    check_cpp_output(&String::from_utf8_lossy(&out.stdout));
    println!("[cpp] 正确性测试通过（3 帧，JSON 结构校验）");
    true
}

fn run_py() -> bool {
    let script = repo_root().join("packages").join("subtitle-ocr-py").join("main.py");
    if !script.exists() {
        eprintln!("[py] main.py 不存在，跳过");
        return false;
    }
    let frames = std::fs::read_dir(frames_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".jpg") || n.ends_with(".png"))
        .collect::<Vec<_>>();
    for f in frames {
        let out = Command::new("python3")
            .arg(&script)
            .arg(frames_dir().join(&f))
            .arg("--subtitle-only")
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let v: serde_json::Value =
                    serde_json::from_slice(&o.stdout).expect("py 输出非法 JSON");
                assert!(v.get("lines").is_some(), "py 输出缺 lines");
            }
            _ => {
                eprintln!("[py] 调用失败（可能未装 rapidocr_onnxruntime）");
                return false;
            }
        }
    }
    println!("[py] 正确性测试通过");
    true
}

fn run_rust() -> bool {
    let bin = repo_root()
        .join("packages")
        .join("subtitle-ocr")
        .join("target")
        .join("release")
        .join("subtitle-ocr");
    if !bin.exists() {
        eprintln!("[rust] 二进制不存在（subtitle-ocr 尚未实现），跳过");
        return false;
    }
    // TODO: rust 实现 CLI 定型后补充 --dir + JSON 结构校验
    eprintln!("[rust] 实现已构建但正确性校验逻辑待补");
    false
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut impl_ = "cpp".to_string();
    for (i, a) in args.iter().enumerate() {
        if a == "--impl" {
            if let Some(v) = args.get(i + 1) {
                impl_ = v.clone();
            }
        }
    }
    let ok = match impl_.as_str() {
        "cpp" => run_cpp(),
        "py" => run_py(),
        "rust" => run_rust(),
        other => {
            eprintln!("未知实现: {other}（可选: {}）", IMPLS.join(" | "));
            std::process::exit(2);
        }
    };
    if !ok {
        eprintln!("[{impl_}] 正确性测试未通过 / 未实现");
        std::process::exit(1);
    }
}
