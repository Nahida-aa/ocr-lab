# subtitle-finder 移植专项

> 目标：完整复刻 VideoSubFinder，功能不丢。当前已知差异见 `DESIGN.md`「已知局限」。

## 已修复

- [x] **CompareTwoSubsOptimal 补 AnalyseImage 带过滤**（`832df55`）
      DifficultCompareTwoSubs2 在 FilterImage(second_filtration)后还有 filter_image
      （逐带裁剪子图用 AnalyseImage 判定无文字则清空）。之前 Rust 漏了这步 → 误判段
      内容变化（bln=0）→ 段提前结束/不保存。加后段数恢复正常（36 段，不再 15-24s 全丢）。

- [x] **has_text 不稳定 → "你开窍..."段 start 偏晚**（`26dab27`）
      - 根因：VideoSubFinder CLI 默认 `g_text_alignment=Center`（IPAlgorithms.cpp:170），
        `second_filtration` 的 Center 偏移块移除 `lb[0]` 时，C++ 把段数组左移一位
        （IPAlgorithms.cpp:2128-2133 `lb[i]=lb[i+1], le[i]=le[i+1]`）让后续
        mpd/mpned 循环看到正确段数组。Rust 之前漏了左移 → 长字幕
        （"你开窍后获得了它的斩击能力"）has_text 抖动 → 段 start 偏晚（实测 20033ms）。
      - 修复：补上 `if ll == 0` 时的段数组左移（filter.rs:413-419）。
      - 实测：段 start 20033ms → **17533ms**（匹配实际字幕出现），段数 38。
      - 顺带澄清两个旧误解：
        1. `g_text_alignment` 默认是 **Center**，不是 Any（CLI 未覆写）。
           之前"Any-skip"方向与 C++ 不一致。commit 5c10a58 切到 Center，
           段数 7→4 对齐 C++，是正确方向。
        2. sobel N-edge `up_l` 系数实为 **10**（IPAlgorithms.cpp:985
           `val=3*val1+10*val2`），不是 7。Rust 的 10 正确，无需改。

- [x] **DifficultCompareTwoSubs2 补 FilterImage 前 ILA 求交**（`7b66412`）
      C++（SSAlgorithms.cpp:2313-2326）在 FilterImage 前把 ImFF1/ImFF2 与各自 ILA 图
      求交（时间掩码）。Rust 之前漏了 → 对未掩码帧过滤 → 保留更多噪声 →
      CompareTwoSubs 易误判内容变化 → 段过度切分（38 段 vs C++ 22）。
      补后段数 38 → 25，关键段收敛：
      - "你开窍..." 由 2 段合并为 1 段（17533,20333，对齐 C++ 17533-20365）。
      - 8033-10467（对齐 8033-10665）、39767-42733（对齐 39766-42765）等合并。

- [x] **检测循环 bln2 用 has_text 而非帧是否存在**（`c95c58a`）
      C++ line 1396 `if (bln2)` 用该帧 has_text 决定 fn_start 走 ddl 还是 2*ddl 步。
      Rust 之前误用 `.is_ok()`（帧存在即 true）→ 无字幕帧也走 ddl 步，检测步进与
      C++ 不一致。改为提级的 `bln2 = f2.has_text`。本次视频输出不变，是步进正确性修复。

## 待办

- [ ] **末尾段 56600-58032 被 Rust 切成 56600-57033 + 57100-58000**（C++ 一整段）
      - 全程对比（C++ end=-1）：前 22 段完全对齐，唯一结构差异在**末尾**。
      - C++ 把 56600-58032（"你现在有两个选择"）当一整段；Rust 在 57100 处切成两段。
      - **严格验证受阻（CPP_SF_NNE dump 被混杂）**：C++ SecondFiltration 的 nNE<mpn
        清空遍布很多行（0-440，共 33156 条），是**主流水线** has_text 判定的正常现象，
        无法单独隔离 compare 路径（DifficultCompareTwoSubs2 的 FilterImage）对 rows
        371-434 的行为。Rust trace12 的 rows 371-434 nNE=7-49 来自 compare 路径。
      - **已确认**：Rust/C++ 的 second_filtration 逻辑一致（都清 nNE<mpn 带），差异在
        compare 路径的具体输入（band lb/le 或 im_ne 或 clear_image）。
      - 需给 C++ FilterImage 加"是否来自 DifficultCompareTwoSubs2"标记，隔离 compare
        路径的 nNE。此前"decoder 差异"结论**未证实**。
      - 假设证伪记录：get_intersect_images 短路非 56600 原因（skip 验证）。

## 已修复（全部对齐 C++）

- ✅ get_intersect_images 短路（`760ac32`）：C++ 任一 has_text=0 帧 → bln=0。消除 52833 误段。
- ✅ 段尾 offset 跑满=DL-1（`e96c7d4`）：C++ for 循环变量跑满后 offset=5。段尾恢复。
- ✅ 段尾 et/pet 真实末帧 PTS（`f31825d`）：字幕最后可见帧 frame(fn+offset-1).pos，
  不虚推。你开窍段尾 20333（OCR 证实字幕 20333 在、20365 无）。
- ✅ 前 22 段结构 + 段尾 PTS 对齐 C++（差 ≤1ms 为取整）。

## 背景 / 已排除

- 参数与 VideoSubFinder 完全一致（mpn=50 / mnthr=0.3 / segh=3 等）
- GetImNE / ApplyModerateThreshold / sobel / color_filtration 结构与 VideoSubFinder 一致
- has_text 逐帧一致（重建 C++ 确认）
- 之前试过 mpn 调低 / mpned Any 跳过 / sobel up_l 改 7 / Any-skip 对齐清理，均基于
  g_text_alignment=Any 的错误前提（实际是 Center），无效/回归。
