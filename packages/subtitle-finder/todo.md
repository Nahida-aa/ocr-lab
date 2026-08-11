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

## 待办

- [ ] **完整对比状态机 FastSearchSubtitles vs run_state_machine**
      - 重点：bln（GetIntersectImages）/ cur_pos-prev_pos 段边界、AnalizeImageForSubPresence
        保存判定、finded_prev / bf / pbf 交互。

## 背景 / 已排除

- 参数与 VideoSubFinder 完全一致（mpn=50 / mnthr=0.3 / segh=3 等）
- GetImNE / ApplyModerateThreshold / sobel / color_filtration 结构与 VideoSubFinder 一致
- 之前试过 mpn 调低 / mpned Any 跳过 / sobel up_l 改 7 / Any-skip 对齐清理，均基于
  g_text_alignment=Any 的错误前提（实际是 Center），无效/回归。
