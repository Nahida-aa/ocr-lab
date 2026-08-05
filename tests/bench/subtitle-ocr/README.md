# subtitle-ocr 基准

对 `subtitle-ocr-cpp` / `subtitle-ocr-py` / `subtitle-ocr`（rust）三个实现做
**正确性灰盒测试**与**性能/质量基准**的驱动包。

两类测试共享抽帧、GT 对齐、JSON 解析等代码，故都用 Rust 编写：

- `src/bin/test.rs` —— 正确性灰盒测试（只跑 3 帧，验证 CLI 契约与输出结构）
- `src/bin/bench.rs` —— 性能与质量基准（整段视频）
- `src/lib.rs` —— 共享算法：`merge_frames`、CER、时序对齐

入口统一走 `justfile`，见 `just --list`。

## 参考素材

| 文件 | 是否入库 | 说明 |
| --- | --- | --- |
| `ref/ocr_manual.json` | ✅ | 人工标注 GT，75 条，含 `start`/`end`（ms） |
| `ref/video_source.mp4` | ❌ **需自备** | 34M，体积过大不入库（`.gitignore`） |
| `../../.test-frames/` | ✅ | 灰盒测试用的 3 帧，460K |

`video_source.mp4` 规格（`bench` 依赖它抽帧，缺失会直接失败）：

```
分辨率  1280x720
帧率    30 fps
时长    170.062993 s
大小    34209105 bytes
sha256  344316821096379954e84a9bc1a437814ab4c5640b44fdb81792545480b76908
```

换用其他视频时 GT 也必须同步替换，否则 CER 与时序指标均无意义。

## 质量指标

`bench` 输出两组互相独立的质量指标，都写进 `summary.json`：

**文本质量**（拼串比对，对齐 LocalDub `eval-ocr.ts`）

- `CER(raw)` —— 去空白后的字符错误率
- `CER(norm)` —— 再做同音字/标点/数字归一化

**时序质量**（按时间重叠配对后统计）

拼串 CER 会把时间戳完全丢掉——字幕整体早/晚 500ms 也照样是 0.36%。
故另按时间区间配对再算一组：

- `IoU(mean)` —— 区间重合度
- `start Δms` / `end Δms` —— 边界偏移的 mean / median / p95，**正值 = 偏晚**
- `paired / missed / spurious / split / merged` —— 对齐计数
- `CER(paired)` —— 配对后逐条 CER，按 GT 字符数加权（漏检记为全错）

### 已知现象

**起点系统性偏晚约 230ms。** `fps=2` 意味着 500ms 的时间分辨率，字幕在两帧
之间出现时要等下一帧才被检出，期望损失恰为半帧。终点偏移则接近 0。当前
只报告不补偿——这个数字正是 fps 选型的依据。

**`zero-dur` 零时长段。** `merge_frames` 对「只在单帧出现」的字幕会产出
`start == end` 的退化区间（实测 341 帧中有 4 条）。这是 LocalDub 原版行为，
此处不修改产出以保持可对比性；但配对时对退化区间改用「点落在区间内」判定，
否则它们会被误计为漏检 + 虚检各一次。

## 性能注意事项

⚠️ **ORT 线程数不是越大越好。** det 输入小、rec 是逐行小图，算子粒度细，
线程超订的调度开销会盖过并行收益。16 核机实测（`--dir`，341 帧）：

| threads | det | rec | RTF |
| --- | --- | --- | --- |
| 2 | 106s | 3.2s | 0.65 |
| 4（cpp 默认） | 127~186s | 3~4s | 0.77~1.12 |
| 16 | 194s | 14.8s | 1.24 |

线程数优先级：CLI `--threads` > env `OCR_INTRA_THREADS` > cpp 内置默认 4。
用 `just scan-threads` 扫描本机最优值。

⚠️ **跑基准前确认机器空载。** 同参数连跑两轮 RTF 波动可达 22%，高负载下测出
的 RTF 没有参考价值。CER 与时序指标则不受负载影响。
