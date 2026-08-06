# 对齐 C++ VideoSubFinder 的经验与结果

> 记录 Rust `subtitle-finder` 与 C++ VideoSubFinder 输出（关键帧段）对齐的排查经验、
> 验证方法、最终结果与坑。目标：让后续避免重复走弯路。

## 一、最终结果（已对齐）

| 段 | Rust `subtitle-finder` | C++ `cli` 分支 |
|---|---|---|
| 段1 | 132-932 | 133-932 |
| 段2 | 932-2265 | 933-2265 |
| 段3 | 2266-3499 | 2266-3499 |
| 段4 | 3700-5033 | 3700-5032 |

- **段数、段边界完全一致**（差 ≤1ms，为解码器帧时序）。
- 测试视频：`/tmp/clip5s.mp4`（5s，30fps，720p，152 帧）。

## 二、发现的三个根因（按排查顺序）

### 1. `g_text_alignment` 默认是 `Center`，不是 Any（最关键）
- **C++ `IPAlgorithms.cpp:170`**：`TextAlignment g_text_alignment = TextAlignment::Center;`
- Rust 实现假设了 Any（params.rs / filter.rs 无 alignment 概念）。
- Center 路径在 `SecondFiltration` 有额外清理：段合并（btd）、中心偏移移除、`mpd`
  最小点密度（S < mpd·SS 时移除）、`mpned` 最小边缘密度（nNE < mpned·S 时移除）。
  Any 跳过这些 → Rust 无法清理噪声 → ISA 过密 → `im_res` 过密 → compare 过度敏感
  → **过度切分（7 段 vs 4 段）**。
- **修复**：`filter.rs::second_filtration` 实现完整 Center 路径
  （`is_too_right`/`farthest_from_center` + 逐条带 `while(1)` 迭代）。
- 效果：SF 从 54157 → 2103（C++ 4579），TF 从 54459 → 6984（C++ 6535）。段数 7→4。

### 2. `get_intersect_images` 交集被空字幕帧清空（段提前结束）
- 窗口 [fn..fn+DL-1] 里若有 `has_text=0` 的空帧（如 fn=105），其全 0 像素把交集清空
  → `analyse_image_flat` 判 false → 段提前结束（段3 从 3499 提前到 3332），
  尽管 fn=100-104 都有字幕。
- **修复**：只交集 `has_text=1` 的帧，跳过空字幕帧。段3 修复到 3499。

### 3. EOF break 不保存末尾段（段丢失）
- 状态机外层 `fn_ >= count` 的 `break` 直接退出循环，没保存进行中的末尾段
  → 段4 丢失。
- **修复**：EOF break 时保存进行中的段（ef/et 定为最后一帧）。

## 三、可靠的验证方法（关键经验）

### ⚠️ 不要用 C++ cli 分支的 `FastSearchSubtitles` dump 做对比
- 它有**帧同步 bug**：`GetTransformedImage` 的 `ImBGR` 与 `ImY` 可能非同一帧
  （并发 `AddGetRGBImagesTask`/`AddConvertImageTask` 覆盖缓冲）。
- 基于它的 dump（BGR 16% 一致等）**不可靠**。

### ✅ 可靠方法：用**相同 BGR 输入**分别喂两边算法
- C++ 独立程序（cli 分支临时写 `edge_dump`/`tf_dump`）：
  - `edge_dump`：读固定 BGR → `GetImNE`/`GetImHE` → dump N/H edge。
  - `tf_dump`：读固定 BGR → `GetTransformedImage` → FF/SF/TF/NE 白点。
  - 需 stub `g_ReportFileName`/`GetFileNameWithExtension` + 链接 `MyClosedFigure.o`。
- Rust 侧用 `#[test] #[ignore]` 读同一 BGR 算对应输出。
- **相同 BGR 时 Rust 与 C++ 的 N/H edge、FF、NE 完全一致（差 <1%）**，证明算法无 bug。

### ✅ 用 ffmpeg CLI 作为 OpenCV 的可靠参照
- `ffmpeg -i clip -frames:v 1 -f rawvideo -pix_fmt bgr24 out.raw`（默认 bt709）
  与 OpenCV VideoCapture 输出 **100% 一致**。
- Rust 的 ffmpeg scaler 默认用 bt601（`sws_getContext` 默认），与 bt709 差 ±1-4，
  但**不影响段数**（尝试 bt709 反而更差，说明不是主因）。

### ✅ 用 `subtitle-ocr` 验证具体帧有无字幕
- `cargo run -p subtitle-ocr --release -- frame.png --subtitle-only`
- 确认 fn=104 有字幕（"这可是剑仙啊"）、fn=105 空；C++ ISA 段3="这可是剑仙响"、段4="不行我得出手了"。
- Rust 与 C++ 的 `has_text`/`TF` 判定在相同帧上**完全一致**。

## 四、坑与注意事项

1. **C++ 全局参数默认值要认真核对**（不只 params.rs 里列的那些）。`g_text_alignment`
   是 Center 而非想当然的 Any，是最隐蔽的坑。
2. **调试日志用 tracing**（`RUST_LOG=subtitle_finder=trace`），`eprintln!` 只在测试里用。
   tracing 输出带 ANSI 颜色码，`grep frame=100` 会匹配不到——先 `sed -r 's/\x1b\[[0-9;]*m//g'`
   去色。
3. **不要在未提交的文件上用 `git checkout -- <file>`**：会整文件回退，丢失未提交工作
   （本会话曾误删 state.rs 的完整状态机，后从 C++ `FastSearchSubtitles` 重新移植）。
4. `get_intersect_images` 的 bln 语义要与 C++ `AND(每帧 has_text)` 对齐：空字幕帧
   （has_text=0）不应参与交集。

## 五、相关提交

- `5c10a58` — second_filtration 实现 Center 路径（段数 7→4）
- `1ba7a5a` — get_intersect_images 跳过空字幕帧 + EOF 保存末尾段（段边界完全对齐）
- `af26cdd` — 调查记录（`.agents/subtitle-finder-cpp-diff.md`）
