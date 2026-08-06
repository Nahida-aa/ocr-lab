# subtitle-finder 与 C++ VideoSubFinder 的对比调查结论

> 目的：记录 Rust `subtitle-finder`（packages/subtitle-finder）与 C++ VideoSubFinder
> 输出差异（Rust 7 段 vs C++ 4 段）的根因调查结论，避免以后重复走弯路。

## 已验证的事实（决定性实验）

### 1. Rust 算法与 C++ 逐像素一致（无算法 bug）
- 用**相同 BGR 帧**分别喂 Rust 和 C++ 的 `get_im_ne`/`get_im_he`：
  - N-edge：C++ 35229 vs Rust 34903（差 <1%）
  - H-edge：C++ 24572 vs Rust 24303（差 <1%）
- Sobel 公式、阈值（mnthr=0.3）、segment 查找、compare 逻辑均已逐行对齐。

### 2. ⚠️ C++ cli 分支有帧同步 bug（重要陷阱）
- `FastSearchSubtitles` 里 `GetTransformedImage` 的 `ImBGR` 与 `ImY` 可能非同一帧
  （并发 `AddGetRGBImagesTask`/`AddConvertImageTask` 覆盖缓冲）。
- 基于 cli 分支 dump 的对比**不可靠**（BGR 16% 一致等数据是假的）。
- 可靠对比必须：独立解码（OpenCV VideoCapture 直接读 vs Rust ffmpeg）+ 相同输入喂两边。

### 3. 解码器色彩差异（已定位但非主因）
- Rust 的 ffmpeg scaler 用 **bt601**（`sws_getContext` 默认），视频实际是 **bt709**。
- OpenCV / ffmpeg CLI 用 bt709。
- Rust 与 OpenCV 的 BGR 仅 ~44.9% 像素一致（差 ±1-4）。
- 手动实现 bt709 转换后 BGR 匹配率提升到 ~55%，**但段数反而从 7 → 10（更差）**。
- **结论：解码器色彩差异不是过度切分的主因。**

### 4. 调阈值无效
- 提高 veple 0.30→0.60：段数反而更多（8），边界移动。
- 说明过度切分不是阈值边界敏感，是结构性差异。

## 当前结论
- 过度切分（7 vs 4）**不是**：
  - 边缘检测算法 bug（已证一致）
  - 解码器色彩差异（bt709 反而更差）
  - 阈值边界（调阈值更差）
- **真正根因（重大发现）：`g_text_alignment` 默认是 `Center`，不是 Any！**
  - C++ `IPAlgorithms.cpp:170`：`TextAlignment g_text_alignment = TextAlignment::Center;`
  - Rust 实现假设了 Any（params.rs / preprocess.rs / filter.rs 无 alignment 概念）。
  - 相同 BGR 对比：C++ FF=58005 SF=**4579** TF=6535；Rust FF=57915 SF=**54157** TF=54459。
    - FF/NE 完全一致（输入算法对齐），但 SF 天差地别：C++ 清理 92%，Rust 只清理 6%。
  - Center 路径在 `SecondFiltration` 有额外清理：合并/移除偏离中心的段、`mpd` 最小点密度
    检查（S < mpd*SS 时移除最远段）、`mpned` 最小边缘密度检查（nNE < mpned*S 时移除）。
    Any 路径跳过这些，导致 Rust 无法清理噪声 → ISA 过密 → im_res 过密 → compare 过度敏感。

## 待办：实现 Center 对齐（SecondFiltration 的 Center 路径）
- `second_filtration` 需加 Center 特有逻辑（mpd/mpned 密度检查 + 中心偏移段移除）。
- 需确认 `analyse_image` 等其他用 g_text_alignment 的函数是否也要 Center。
- C++ 有 32 处 g_text_alignment 使用，需逐一评估。

## 现状
- Rust 当前输出（bt601 scaler 基线）：7 段
  `133-332, 333-832, 966-1332, 1333-1832, 1832-2199, 2266-3332, 3933-4899`
- C++（cli 分支）：4 段
  `133-932, 933-2265, 2266-3499, 3700-5032`
- frame.rs 已还原到基线（bt601 scaler），工作区干净。

## 排查工具备忘
- Rust 调试日志：`RUST_LOG=subtitle_finder=trace`（decode frame isa_wc / 边缘图白点 /
  dilate(NE) / second_filtration step / compare 详情 / 内容变化判定帧）。
- C++ 独立对比程序：cli 分支写 `edge_dump`（读固定 BGR → GetImNE/GetImHE → dump），
  需要 stub `g_ReportFileName`/`GetFileNameWithExtension` + 链接 MyClosedFigure.o。
- 用 ffmpeg CLI 转 BGR 作为 OpenCV 的可靠参照（默认 bt709 与 OpenCV 100% 一致）。
