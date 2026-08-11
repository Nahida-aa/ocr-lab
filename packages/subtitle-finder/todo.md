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

- [ ] **完整对比状态机 FastSearchSubtitles vs run_state_machine**
      - 重点：bln（GetIntersectImages）/ cur_pos-prev_pos 段边界、AnalizeImageForSubPresence
        保存判定、finded_prev / bf / pbf 交互。

- [ ] **sobel N-edge up_l 系数**（我们 10 vs VideoSubFinder 7），确认与状态机差异叠加。

## 背景 / 已排除

- 参数与 VideoSubFinder 完全一致（mpn=50 / mnthr=0.3 / segh=3 等）
- 已试 mpn 调低 / mpned Any 跳过 / sobel up_l 系数 / Any-skip 对齐清理，
  均未完全修复（见 DESIGN.md「已知局限」）
- GetImNE / ApplyModerateThreshold 结构与 VideoSubFinder 一致
