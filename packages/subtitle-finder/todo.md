# subtitle-finder 移植专项

> 目标：完整复刻 VideoSubFinder，功能不丢。当前已知差异见 `DESIGN.md`「已知局限」。

## 已修复

- [x] **CompareTwoSubsOptimal 补 AnalyseImage 带过滤**（`832df55`）
      DifficultCompareTwoSubs2 在 FilterImage(second_filtration)后还有 filter_image
      （逐带裁剪子图用 AnalyseImage 判定无文字则清空）。之前 Rust 漏了这步 → 误判段
      内容变化（bln=0）→ 段提前结束/不保存。加后段数恢复正常（36 段，不再 15-24s 全丢）。

## 待办

- [ ] **has_text 不稳定 → "你开窍..."段 start 偏晚**（仍存在）
      - filter_image_analyse 修了段内容误判，但 "你开窍..."段 start 仍 20033（应 17.5s）。
      - 根因：second_filtration 的 mpd（点密度）在 Any 下被我们执行（VideoSubFinder
        line 2014 `if (g_text_alignment != Any)` 跳过），清空字幕条带 → has_text 抖动。
      - **已用 VideoSubFinder 重编输出 has_text（16.5-21s，163 帧）确认其模式**：
        字幕区（17.3-20.4s "你开窍..."）**稳定 1**；段边界无字幕区 fn=23(17267ms)、
        fn=116-126(20367-20700ms) 为 0。
      - 我们差异：原始版字幕区抖动（部分 0）→ start 偏晚；Any-skip 全 1（连段边界 0
        也没了）→ 状态机段边界错乱、丢段。
      - **需对齐 VideoSubFinder 的 has_text = 字幕区稳定 1 + 段边界 0**。确认我们
        Any-skip 在段边界（20367-20700ms）为何判 1（是 color_filtration n==0 还是
        n_ne<mpn 未触发）。
      - **段边界 has_text 差异**：VideoSubFinder 在"你开窍..."结束后 20367ms 立即 0
        （连续 11 帧）；我们 Any-skip 20367-20633 仍 1、20667 才 0（晚 ~300ms）。
        → 段 end 偏晚。需确认我们 20367-20633 帧为何判 1。
      - **color_filtration 段边界差异**：20367-20600ms 我们 color_filtration 找到 n=1
        文字带 → has_text=1；VideoSubFinder 同帧 has_text=0。差异可能在 color_filtration
        （我们误判无字幕帧的残影/背景为文字带）或 second_filtration（未移除该 n=1 带）。
        需对比 VideoSubFinder 同帧的 color_filtration n 定位。

- [ ] **完整对比状态机 FastSearchSubtitles vs run_state_machine**
      - 重点：bln（GetIntersectImages）/ cur_pos-prev_pos 段边界、AnalizeImageForSubPresence
        保存判定、finded_prev / bf / pbf 交互。

- [ ] **sobel N-edge up_l 系数**（我们 10 vs VideoSubFinder 7），确认与状态机差异叠加。
      - ✅ **2026-08-11 已核实源码**：C++ `FastImprovedSobelNEdge`（IPAlgorithms.cpp:985）
        `val = 3*val1 + 10*val2`，up_l 系数是 **10**，不是 7。我们 Rust（sobel.rs:157 `+10*(up_l-dn_r)`）
        也是 10 → **一致，无需改**。此前 todo/DESIGN 记的"7"是误读，待更新 DESIGN。

## 背景 / 已排除

- 参数与 VideoSubFinder 完全一致（mpn=50 / mnthr=0.3 / segh=3 等）
- 已试 mpn 调低 / mpned Any 跳过 / sobel up_l 系数 / Any-skip 对齐清理，
  均未完全修复（见 DESIGN.md「已知局限」）
- GetImNE / ApplyModerateThreshold 结构与 VideoSubFinder 一致
- **✅ 2026-08-11 重大澄清：`g_text_alignment` 默认是 `Center`（IPAlgorithms.cpp:170），
  不是 `Any`**。CLI（cli_main.cpp）未设置它 → 用户跑的 VideoSubFinder 实际是 **Center**
  对齐，不是 Any。因此之前"Any-skip 对齐"的方向（Design line 215"本实现固定 Any"、
  跳过 line 2014/2208/2357）是**错的**——它把 Rust 改成 Any，反而不对齐 C++ 的 Center。
  正确方向：Rust `second_filtration` 应实现 **Center**（commit 5c10a58 已做，段数 7→4
  对齐 C++）。has_text 抖动需在 Center 模式下继续排查（不是 Any）。
