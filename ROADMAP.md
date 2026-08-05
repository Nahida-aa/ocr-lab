# ocr-lab 路线图

## 目标

单仓库聚合**三个强相关的独立目标**（非包含关系，各自有独立交付物与验收）：

1. **字幕识别**（目标 1）：从视频帧抽取字幕文字。
   - 实现变体：`packages/subtitle-ocr-cpp`（C++/ORT 直连，已实现）、
     `packages/subtitle-ocr-py`（Python/rapidocr，已实现）、
     `packages/subtitle-ocr`（Rust，待实现）。
   - 横比基准：`tests/bench/subtitle-ocr`（`bin/test.rs` 正确性 + `bin/bench.rs` 性能占位）。
   - 参考素材：`tests/bench/subtitle-ocr/ref/`（video_source.mp4 + ocr_manual.json 人工标注）。
2. **GUI 自动操作**（目标 2）：「看屏幕 → 理解 → 操作」闭环。
   - 组合：`crates/capturer`（抓图）+ `crates/rapidocr-ort`（文字识别）+ `crates/screen-operator`（点击/输入回灌）。
3. **GUI 自动化测试**（目标 3）：可复现 fixture + 识别/操作验证。
   - `tools/gen_ui_img`（gpui 真实渲染 → capturer 抓图）→ `tests/fixtures/ui_*.png`。

三者共享底层 OCR 引擎与抓图/注入设施（故同仓），但**目标 1 的基准只服务字幕识别，
不可误用作目标 2/3 的验收**。

```
                        ┌─────────────────────────────────────────┐
                        │  共用底层：rapidocr-ort / capturer / 注入  │
                        └─────────────────────────────────────────┘
            ┌──────────────────────┬──────────────────────┬──────────────────────┐
            ▼                      ▼                      ▼
     目标1: 字幕识别         目标2: GUI 自动操作      目标3: GUI 自动化测试
     subtitle-ocr-*          capturer+rapidocr-ort    gen_ui_img → fixtures
     + bench/subtitle-ocr    + screen-operator        + 识别/操作验证
```

## 当前状态

### ✅ rapidocr-ort（文字识别，可用）
- `OcrEngine` 支持 v3 / v6-tiny / v6-medium，**运行时 `--model` 切换**（非条件编译）。
- 检测：DB 概率图 + 二值化 + 连通域 + 框扩张（PP-OCR det 概率图只激活文字行
  中间带，需外扩才能包住完整字形）。
- 识别：CRNN/CTC 贪婪解码，时间轴自动判定 `[1,T,C]` / `[1,C,T]`。
- 二进制 `rapidocr-ort` 对单图输出 JSON（`text` / `score` / `bbox` / `center`）。
- `tests/`：可复现 fixture + `v3_detects_text_on_fixtures` 集成测试。
- `tools/gen_fixtures.py`：用 Noto Sans CJK 生成文字图片（支持中文，避免 tofu）。

### ⚠️ 已知问题 / 待修
- **v3 识别仍会掉字**：长文本中间/末尾字符偶发丢失（32px 输入分辨率限制 +
  框扩张比例敏感）。需继续调参：`--model` 之外，扩张比例可经 `OCR_EXPAND`
  环境变量覆盖调试。
- **中文测试图疑似渲染异常**：`tools/gen_fixtures.py` 用 `.ttc` 字体集合，可能
  取到了非预期 face，导致中文图视觉有问题。需用真实渲染核对（见下）。
- **v6 rec 预处理未完成**：v6 det 工作，但 rec 归一化与 v3 不同，当前为占位
  （0.5/0.5），识别不准，标记实验性。需补 PP-OCRv6 专用预处理。
- `cls` 方向分类已加载未使用（旋转文本待支持）。

## 下一步（建议顺序）

1. **修中文 fixture 渲染**：确认 `gen_fixtures.py` 取到 Noto Sans CJK SC 正确
   face，肉眼核对 `tests/fixtures/zh1.png` / `mix1.png` 确实为中文而非乱码/方块
   （已定位 `tools/gen_fixtures.py:84` 的 `h, w = canvas` 维度反了 bug，已修）。
2. **用 gpui 真实渲染生成 fixture**（见下）：`tools/gen_ui_img` 开全屏 gpui 窗口画
   文字，`crates/capturer` 通过 xdg-desktop-portal 抓屏裁切存 PNG。比 PIL 假图更
   贴近真实 GUI，且把「渲染→抓图→存 PNG」链路跑通（将来模拟操作复用）。
3. **提升 v3 识别完整度**：在 fixture 上做参数扫描（扩张比例、rec 输入高度、
   是否双线性），目标是 `Hello OCR`→`Hello OCR`、`你好世界`→`你好世界` 不丢字。
4. **补 v6 rec 预处理**，让 `--model v6-*` 与 v3 同等可用。
5. **opencv 视觉层**：版面/图标/状态识别，作为文字层的补充。
6. **ui_probe**：把 OCR 结果接到操作回灌（点击 center、输入文本、断言期望文字），
   先打通 waydroid 截图 → OCR → 断言 的最小闭环。
7. **(可选) yolo 控件检测**：对复杂 UI 做控件级定位，降低纯 OCR 的误判。

## 抓图基础设施（crates/capturer）

- 自研跨 compositor 抓图 crate，抽象 `Capturer` trait。
- 当前实装后端 `PortalCapturer`：基于 `ashpd` 的 xdg-desktop-portal Screenshot
  （非交互、全屏），抓到文件后在 Rust 里按已知区域裁切。KDE/GNOME/wlroots 都
  实现 portal，写一次通用。
- 预留后端（TODO，按环境接入）：wlroots `zwlr_screencopy_manager_v1`
  （smithay-client-toolkit）、X11（x11rb）、Android(waydroid)。
- 注意：gpui 在 Linux/Wayland 无离屏出 PNG 的官方 API（仅 macOS 有
  `render_scene_to_image`），故走「真窗口 + portal 抓屏」路线。

## 目录结构

```
ocr-lab/
├── Cargo.toml              # workspace（crates/*）；[patch.crates-io] 覆盖 gpui 到 zed d88f682
├── ROADMAP.md              # 本文件
├── models/rapidocr/        # 权重（gitignore，152M）
├── crates/
│   ├── rapidocr-ort/       # 检测+识别 库 & 二进制（目标 2/3 共用的识别引擎）
│   ├── capturer/           # 自研跨 compositor 抓图基础设施（Capturer trait + portal 后端）
│   ├── screen-operator/    # 操作回灌层（目标 2）
│   ├── util/               # 资源加载（rust-embed 辅助）
│   └── settings/           # 配置（rust-embed 资源）
├── packages/
│   ├── subtitle-ocr-cpp/   # 目标 1：字幕识别 C++ 实现（ocr.test.ts + test.justfile）
│   ├── subtitle-ocr-py/    # 目标 1：字幕识别 Python 实现
│   └── subtitle-ocr/       # 目标 1：字幕识别 Rust 实现（待实现）
├── tools/
│   ├── gen_fixtures.py     # 文字图片生成器（PIL/中文，确定性单元 fixture）
│   └── gen_ui_img/         # gpui 真实渲染 → capturer 抓图 → 存 tests/fixtures/ui_*.png（目标 3）
├── tests/
│   ├── fixtures/           # 测试图（stable1/big1/nat1/zh1/mix1 …；ui_* 为 gpui 生成）
│   ├── .test-frames/       # 正确性测试用的 3 帧（目标 1 跨实现测试源）
│   └── bench/subtitle-ocr/ # 目标 1 横比基准（Cargo 包 bench-subtitle-ocr）
└── assets/                 # 嵌入资源（settings/keymaps）
```
