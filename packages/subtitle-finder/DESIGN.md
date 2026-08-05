# subtitle-finder

吃视频 → 输出字幕关键帧图 + 时间轴。**复刻 VideoSubFinder 的「筛选」管线**（传统 CV，
不做 OCR），把 `rapidocr-ort` 作为下游识别器，对应官方
`VideoSubFinder → RapidVideOCR` 的分工。

**本包只负责"找到字幕变化的关键帧 + 时间轴"，不做识别。**

## 为什么（动机）

- 现有 `subtitle-ocr` 是**纯单帧 OCR**，抽帧在 bench（测试驱动）。
- bench 抽帧用 fps=2（341 帧），有 500ms 采样偏移。
- VideoSubFinder 逐帧（30fps）处理 + 传统 CV 筛选，只在字幕变化时输出关键帧——
  时间无偏移、且大幅减少要 OCR 的帧数。
- 完美复刻它的管线，需要一个吃视频、输出关键帧图的包，与识别解耦。

## 输入 / 输出契约

```
输入：视频文件（mp4 等，ffmpeg 可解码）
输出：
  - 关键帧图目录（每张 = 一个字幕段的一张代表帧）
  - 每张文件名带 起止时间（对齐 VideoSubFinder：`{h}_{m}_{s}_{ms}__{h}_{m}_{s}_{ms}`）
  - 或结构化 JSON：{ 关键帧路径, start_ms, end_ms, text_img }
```

下游：`rapidocr-ort` 对关键帧图 rec，得到字幕文本（对应 RapidVideOCR）。

## 核心算法（复刻自 VideoSubFinder `SSAlgorithms.cpp`）

### 1. 逐帧解码（30fps）
VideoSubFinder 用 ffmpeg/OpenCV 逐帧 `OneStep()` 读入内存处理，不落盘。
**复刻：逐帧解码，每帧作为 `Array3<u8>`（BGR）喂给后续。**

### 2. DL=6 滑动窗口 + 相邻帧交集（去噪）
`FastSearchSubtitles` 用 `ddl = DL/2 = 3` 步进，预取 DL 帧进缓冲区。
对相邻两组帧（`fn+ddl-1` 与 `fn+2ddl-1`）做 `IntersectTwoImages`（逐像素交集），
去掉单帧闪烁/噪声，只留稳定字幕像素。
**复刻：滑动窗口 + 逐像素交集（可用 `geometry::imgproc` 的 SIMD 加速）。**

### 3. AnalyseImage（水平投影检测文字）
按 `g_segh=3` 高的水平条带，统计每列白色像素密度，用 `g_tp=0.3`（文字占比）、
`g_mtpl=0.022`（最小文字长度）判断该帧是否有文字行。
**复刻：水平条带投影 + 阈值判定。**

### 4. FilterTransformedImage（判断"有字幕"）
`AnalizeImageForSubPresence` → `FilterTransformedImage`：找连通域图元（文字块），
按尺寸/密度过滤，判断是否有字幕。
**策略：用 OpenCV `findContours`（仓库已依赖）替代手写连通域（CMyClosedFigure），
保留过滤规则（尺寸/密度阈值）。**

### 5. CompareTwoSubs / CompareTwoSubsOptimal（跨帧比较，判断字幕是否变化）
对两帧（与各自 ILA 交集后）逐像素比较差异比例，用 `g_veple=0.30`/`g_ilaple=0.30`
阈值判断字幕内容是否变化。
**复刻：逐像素差异比例 + 阈值。**

### 6. FastSearchSubtitles 状态机（时间轴核心）
用 `bf/ef`（起止帧）、`bt/et`（起止时间）、`DL`、`g_max_dl_down=20`/`g_max_dl_up=40`
跟踪字幕段：字幕出现（bf）、持续、变化（新字幕）、消失（ef）。只有**字幕内容变化**
才输出关键帧。
**复刻：状态机，这是"时间无偏移"的关键，必须精确对齐。**

## 关键参数（默认值，需对齐 VideoSubFinder）

| 参数 | 值 | 含义 |
| --- | --- | --- |
| `DL` | 6 | 字幕帧长度（滑动窗口） |
| `segh` | 3 | 水平条带高度 |
| `tp` | 0.3 | 文字占比阈值 |
| `mtpl` | 0.022 | 最小文字长度（百分比） |
| `veple` | 0.30 | 跨帧文字差异阈值 |
| `ilaple` | 0.30 | ILA 差异阈值 |
| `max_dl_down` | 20 | 字幕最短持续（帧数） |
| `max_dl_up` | 40 | 字幕最长持续（帧数） |

## 模块划分（Rust）

```
packages/subtitle-finder/
  src/
    lib.rs          # 对外 API：find_keyframes(video) -> Vec<Keyframe>
    frame.rs        # 逐帧解码（视频 → 帧流）
    preprocess.rs   # 交集 / ILA-ISA 图生成 / AnalyseImage 投影
    filter.rs       # FilterTransformedImage（用 OpenCV findContours 替代连通域）
    compare.rs      # CompareTwoSubs / CompareTwoSubsOptimal（跨帧比较）
    state.rs        # FastSearchSubtitles 状态机（时间轴）
    params.rs       # 全局参数（对齐上表）
    export.rs       # 输出关键帧图 + 时间轴（文件名/JSON）
```

## 复用策略

| 算子 | 来源 |
| --- | --- |
| 逐像素交集 / 投影 | `geometry::imgproc`（SIMD） |
| 连通域（文字块） | OpenCV `findContours`（替代手写 CMyClosedFigure） |
| 图像读取 | `image` crate（转 BGR） |
| 视频解码 | **待定**（见下） |
| 识别 | `rapidocr-ort`（下游，不在本包） |

## 视频解码方案（待你决策）

仓库没有 Rust 视频解码库（bench 用外部 ffmpeg 命令抽帧）。逐帧 30fps 解码两个选择：

1. **`ffmpeg-next` crate**：真正逐帧解码，性能好，但引入 FFmpeg 系统依赖 + 绑定。
2. **外部 ffmpeg 命令**：复用 bench 的抽帧方式，但只能按 fps 抽帧、有进程启动开销，
   且难做到"逐帧"。

建议方案 1（`ffmpeg-next`），才能做到 30fps 逐帧复刻 VideoSubFinder 的时间行为。
若你不想引 FFmpeg 依赖，需接受抽帧粒度与 VideoSubFinder 不完全一致（时间偏移回来）。
