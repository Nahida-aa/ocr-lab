# ocr-lab

通用的**实时屏幕分析与操作回灌**工具链原型。对任意 GUI（包括自绘框架如
gpui）的截图做文字识别，再把识别结果映射成可执行的点击/输入操作，形成
「看屏幕 → 理解 → 操作」的闭环。不针对某一框架训练专用 OCR，而是用通用
视觉能力。

详见 [ROADMAP.md](./ROADMAP.md)。

## 组成

| 路径 | 作用 |
| --- | --- |
| `crates/rapidocr-ort` | PP-OCR 文字识别（基于 ONNX Runtime）。`det` 检测 + `rec` 识别 + `cls` 方向分类，DB 后处理对齐官方实现。 |
| `crates/capturer` | 屏幕/窗口抓图基础设施。含 `PortalCapturer`（xdg-desktop-portal Screenshot，全屏）与 `ScreenCastCapturer`（ScreenCast + PipeWire，可选窗口且不受遮挡）。 |
| `tools/gen_ui_img` | 用 gpui 真实渲染文字卡片，再经 `capturer` 抓图，生成 OCR 测试 fixture。 |
| `models/rapidocr` | PP-OCR v3 / v6 的 ONNX 权重与字典（需自行放置，见下）。 |

## 识别结果结构

`rapidocr-ort` 对每个文本框输出：

```jsonc
{
  "text": "Count:100",
  "confidence": 0.95,   // 识别置信度（rec 分支平均字符概率）
  "score": 0.80,        // 检测框得分（框内平均概率）
  "box": [[x,y],[x,y],[x,y],[x,y]],  // 四点（顺时针：左上、右上、右下、左下）
  "center": [x, y],     // 四点平均的几何中心，便于点击回灌
  "x_range": [min_x, max_x],   // 横向值域，便于按列/区域过滤
  "y_range": [min_y, max_y]    // 纵向值域，便于按行/区域过滤
}
```

## 构建

```bash
cargo build --workspace
```

依赖：Rust 工具链 + 系统库 `libopencv`（抓图/后处理用到 `imgproc`）+ ONNX
Runtime（`ort` crate 自带）。`capturer` 的 ScreenCast 路径还需要
`libpipewire-0.3`（已在多数 KDE/GNOME 桌面预装）。

## 准备模型权重

`models/rapidocr/` 下需放置 PP-OCR 权重与字典（从
[PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) 导出为 ONNX）。默认查找：

- v3：`ch_PP-OCRv3_det_infer.onnx` / `ch_PP-OCRv3_rec_infer.onnx` /
  `ch_ppocr_mobile_v2.0_cls_infer.onnx` / `ppocr_keys.json`
- v6 tiny / medium：`pp-ocrv6_{tiny,medium}_{det,rec}.onnx` + 对应 `.txt` 字典

## 用法

### 1. 对图片跑 OCR

```bash
cargo run -p rapidocr-ort -- --model v3 tests/fixtures/ui_stable1.png
```

输出 JSON，含 `model` / `image` / `width` / `height` 与每个文本框的
`results`（结构见上）。

支持 `--model v3|v6-tiny|v6-medium`，以及 `--model-dir <dir>` 指定权重目录。

### 2. 生成 OCR 测试 fixture

`tools/gen_ui_img` 用 gpui 渲染文字卡片并抓图，产出与 `rapidocr-ort`
输入对齐的 fixture：

```bash
# 批处理全部内置 fixture（每个单独起进程，干净退出）
cargo run -p gen_ui_img

# 单个 fixture
cargo run -p gen_ui_img -- --capture ui_stable1

# 仅渲染、不抓图（窗口常驻 10s 供肉眼核对）
cargo run -p gen_ui_img -- --gui ui_stable1
```

抓图后端为 `ScreenCastCapturer`：首次会弹对话框让你选 gpui 窗口，选完后
`restore_token` 存入 `tests/fixtures/.capture_token`，后续运行免对话框
（即「提前赋权」）。也可 `--token <t>` 显式传入，或 `--reset-token` 强制重选。

> 注意：ScreenCast 抓图需在真实 Wayland/X11 桌面会话中运行，且窗口本身
> 即为卡片（760×260），故抓到的就是窗口本体、无需裁切。

### 3. 直接抓图（不跑 OCR）

```bash
# 全屏
cargo run -p capturer --bin capturer_cli -- full --out capture.png
# 区域
cargo run -p capturer --bin capturer_cli -- region 120 120 720 220 --out capture_region.png
```

## 已知限制

- `capturer` 的 ScreenCast 后端在 OpenCV 5 环境下退化为**轴对齐包围盒**
  （`min_area_rect` 在该组合下无绑定）；近水平文本不受影响，旋转文本支持
  需 OpenCV 4。
- 当前为 CLI / 库原型，尚未接入「识别结果 → 模拟操作」的回灌执行层。
