# Rust vs C++ 状态机对比与优化空间分析

> 记录 Rust `subtitle-finder` 与 C++ VideoSubFinder 状态机的性能对比、Rust 侧
> 的分配热点与优化空间。目标：指导后续优化，避免重复分析。

## 一、性能基线（2026-08-07 实测）

用 `/tmp/clip5s.mp4` 循环生成的 30s / 870-884 帧视频（1280×720）对比：

| 配置 | 墙钟 | 每帧 |
|---|---|---|
| **Rust 单线程**（subtitle-finder） | 17.6s | **~19.9ms** |
| **C++ `--threads 1 --no-save`** | 17.8s | **~20.5ms** |
| **C++ 默认（12 线程）** | 15.8s | **~18.2ms** |

- Rust 与 C++ **单核算法效率相当**（~20ms/帧），无需为"追上 C++"而优化。
- **C++ 1→12 线程只快 1.12x**：瓶颈是顺序状态机逻辑 + 帧同步/事件等待
  （`evt_rgb.wait()`、`shared_custom_task`、TBB 流水线同步），不是可并行的帧变换。
  → 给 Rust 加多线程收益预计也有限。协程对 CPU-bound 无益（无 I/O 等待，async 只有调度开销）。

> 注：C++ CLI 已加 `--no-save`（`g_disable_save_images`）与 `--threads N`（`g_threads`）
> 参数，在 `cli/cli_main.cpp`（该 `cli/` 目录未跟踪进 git）。重建见本文档末尾附录。

## 二、Rust `--profile` 分阶段耗时

`--profile` 只统计了**每帧变换**（`get_transformed_image` 各阶段）：

| 阶段 | 总耗时 | 每帧 |
|---|---|---|
| filter(连通域) | 8.0s | 9.1ms |
| im_ff(边缘+阈值) | 6.4s | 7.2ms |
| bgr_to_yuv | 2.0s | 2.2ms |
| color_filtration | 1.2s | 1.4ms |
| 总计 | 17.6s | 19.9ms |

**关键**：profile 未统计状态机逻辑（compare/筛选/解码）的耗时。而状态机每帧
**额外分配 13-15MB** 的整帧深拷贝（见下），这些分配完全在 profile 统计之外，却
是真实开销（malloc 1280×720 Vec 是 O(n)，会触发清零/页错误）。

## 三、C++ vs Rust 状态机：缓冲复用差异

帧尺寸：1280×720 = 921,600 px。单缓冲：`im/ne`(u8)≈0.92MB，`y`(u16)≈1.84MB，
`bgr`(×3)≈2.76MB。

| 方面 | C++ | Rust |
|---|---|---|
| 帧变换结果 | `ImForward[DL]/ImNEForward[DL]/ImYForward[DL]` 环形缓冲，**复用槽位** | `FrameCache` 滑窗 VecDeque，已复用（OK） |
| 状态变量 | `ImIntS/ImNES/ImYS/ImFS` 持久缓冲，仅更新时赋值 | `im_int_s` 等持久 Vec，已复用（OK） |
| `get_intersect_images` | 用环形缓冲槽引用 + 单个 `ImInt/ImYInt` 工作缓冲 | **每帧新分配 `Vec<Vec<u8>>` + 多次整帧 clone** ❌ |
| `compare` 临时图 | `ImRES/ImFF1/ImFF2` 在调用处声明、可跨调用复用 | **每次调用 `to_vec()/clone()` 全新分配** ❌ |
| **每帧额外分配** | **~0**（复用工作缓冲） | **13-15MB/帧**（未被 profiler 统计） |

## 四、Rust 分配热点（按收益排序）

### 1. `get_intersect_images`（state.rs:238-279）—— 每帧调 1 次，省 4-6MB/帧
- `ims.push(f0.im.clone())` + `imys.push(f0.y.clone())`（253-255）：2 次整帧深拷贝
- 每个 has_text 帧再 `im.clone()/y.clone()`（259-261）：2 次/帧
- `im_int = ims[0].clone()` + `y_int = imys[0].clone()`（274/277）：再 2 次
- **可改**：收集 `Vec<&[u8]>`（引用切片）而非 `Vec<Vec<u8>>`；`im_int` 用调用方
  传入的复用缓冲，或直接用第一个有字幕帧切片 + 就地 intersect。
  省 ~4-6MB/帧。

### 2. `compare_two_subs_optimal`（compare.rs:313-348）—— 每段结束扫描时频繁调，省 ~8MB/次
每次调用内部整帧分配：
- `im_ff1=im1.to_vec()` + `im_ff2=im2.to_vec()`（234-235）：2×0.92MB
- `add_two_images`（imgops.rs:65 内部 `a[..size].to_vec()`）：+0.92MB
- `im1_c=im_ff1.clone()`（275）、`im2_c=im1_c.clone()`（280）：2×0.92MB
- `ila_int=ila1.to_vec()`（282）：1.84MB（u16）
- `dilate`（287）：内部 to_vec + 每迭代 clone → ~1.84MB
- 首轮 false 走 DifficultCompare：`ff1/ff2=im1/im2.to_vec()`（340-341）2×0.92MB +
  `filter_image` 内 `prev=im_f.to_vec()` **循环**（362，通常 2-3 轮）→ 2-3MB
- **一次 optimal ≈ 8-12 个整帧分配 ≈ 8~11MB**
- **可改**：`im_res`/`im_ff1`/`im_ff2` 用函数内复用缓冲（`Vec::with_capacity` 传出），
  `get_lines_info` 复用 `lb/le`。这是最大的单次调用开销。

### 3. `prev_im_ne` 每帧 1 次 clone（state.rs:798，省 0.92MB/帧）
- `prev_im_ne = get_frame(&cache, fn_)?.ne.clone()` —— 每帧必执行的整帧 ne 拷贝。
- **可改**：双缓冲乒乓（两个 preallocated Vec 轮流当 prev）。

### 4. 追踪循环里 `p_prev_ne`/`ne_ff` 每 off clone（state.rs:697/698/714/725）
- 字幕段结束扫 `off in 0..(dl-1)` 时，每 off 循环 `p_prev_ne=prev_im_ne.clone()`/
  `p_prev_ne=f_off.ne.clone()`（0.92MB × dl）。**可改**：仅首次 clone，其余复用。

### 5. 检测阶段 `get_frame(...)?.clone()` 整帧（state.rs:469/476）
- `f1 = get_frame(&cache, ...).clone()` clone 整个 FrameData（含 2.76MB bgr），
  但调用方只用 `has_text/im/y`。**可改**：只 clone 所需字段或直接传引用。

### 6. `compare_by_offset`（state.rs:302/304/325-326）
- `ne12 = prev_ne.to_vec()`、`im_int2=f_off.im.clone()` 等，`ne12` 仅只读可传 `&prev_ne`。

## 五、建议优化顺序

1. **`get_intersect_images` 引用化**（省 4-6MB/帧，收益最大、改动局部、风险低）
2. **`compare_two_subs_optimal` 去冗余拷贝**（省 ~8MB/次，单次最大）
3. **`prev_im_ne` 双缓冲乒乓**（省 0.92MB/帧，每帧必省）
4. 追踪循环 `p_prev_ne` 复用 + 检测阶段去整帧 clone

> 每步改完跑 `clip5s` 验证段仍为 `132-932/932-2265/2266-3499/3700-5033` 4 段不变。

## 六、实测验证（2026-08-07）——分配优化对墙钟影响甚微 ⚠️

### 6.1 整视频 A/B（高负载机器，load avg 2.1+）

实现第 1 项（`get_intersect_images` 引用化，改收集 `Vec<&[u8]>` 省 ~4-6MB/帧）后
A/B 对比：

| 版本 | 3 次墙钟（30s/884 帧视频） | 均值 |
|---|---|---|
| 优化前 | 35.03s / 34.41s / 33.60s | ~34.3s |
| 优化后（第1项） | 34.20s / 35.30s / 35.00s | ~34.5s |

**差异 ~0.2s（0.6%），在噪声范围内。**

### 6.2 单函数微基准（`src/bin/perfbench.rs`，隔离、负载稳健）✅ 决定性

整视频 A/B 受负载干扰不可靠，故用 `std::time::Instant` 隔离测关键函数
（1280×720 合成字幕帧，多次取均值）：

| 函数 | 每调用 | 相对 20ms/帧 |
|---|---|---|
| `compare_two_subs_optimal` **相同帧**（追踪热路径） | **0.49 ms** | ~2.5% |
| `compare_two_subs_optimal` **差异帧**（触发 DifficultCompare） | **5.06 ms** | ~25%（仅段边界） |
| `imgops::dilate(iters=1)` | 0.52 ms | — |
| `imgops::add_two_images` | 0.033 ms | — |
| `imgops::intersect_two_images_inplace` | 0.033 ms | — |

**结论（决定性）：**
1. 状态机 compare 每帧成本 ~0.5ms（2.5%），**分配优化顶多省零点几 ms，墙钟不可测**。
   差异帧 5ms 只在字幕变化边界出现，非每帧。
2. **`compare` 耗时被 `dilate`（0.52ms）主导**，不是分配本身——但都远小于变换。
3. **真正的瓶颈仍是每帧变换**（filter 9ms + im_ff 7ms ≈ 16ms 占 80%）。

> 微基准跑法：`cargo run -p subtitle-finder --release --bin perfbench`。
> 单函数隔离测是判断「某优化是否真有用」的可靠手段（负载机整视频 A/B 不可靠）。

## 七、结论与建议

- **状态机分配 churn 不是墙钟瓶颈**，第 2/3/4 项优化（compare 去拷贝、prev_im_ne
  双缓冲等）**不必做**——不会带来可测提速。
- `get_intersect_images` 引用化已实现并验证正确（输出不变、测试通过），作为
  **代码质量改进**保留，但不要期待墙钟收益。
- **要提速，聚焦变换本身**：`filter`（连通域，9ms）与 `im_ff`（边缘+阈值，7ms）。
  这两块是 CPU-bound，可考虑 SIMD / 多线程 / 算法简化。多线程有收益但要处理状态机
  顺序依赖（C++ 12 线程仅 1.12x，瓶颈是顺序状态机）。

## 附录：C++ CLI 重建命令

`cli/` 目录未跟踪进 git，改 `cli_main.cpp` 后重建：
```sh
cd cli
g++ -O2 -std=c++17 -I../Components/IPAlgorithms -I../Components/Include \
  -I../Interfaces/VideoSubFinderWXW -I/usr/include/opencv5 -I. \
  -include opencv_compat.h -c cli_main.cpp -o build/cli_main.o
g++ -O2 -std=c++17 build/cli_main.o build/IPAlgorithms.o build/SSAlgorithms.o \
  build/MyClosedFigure.o -o video_subfinder_cli \
  -lopencv_core -lopencv_imgproc -lopencv_imgcodecs -lopencv_videoio \
  -lopencv_geometry -ltbb -lpthread
```
（OpenCV 5 头在 `/usr/include/opencv5`，需 `-include opencv_compat.h` 垫 CV_IMWRITE_* 常量。）
