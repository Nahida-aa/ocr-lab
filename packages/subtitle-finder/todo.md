# subtitle-finder 移植专项

> 目标：完整复刻 VideoSubFinder，功能不丢。当前已知差异见 `DESIGN.md`「已知局限」。

## 待办

- [ ] **完整对比状态机 FastSearchSubtitles vs run_state_machine**
      - 问题：Any-skip 修复后 has_text 在 17-20.5s 稳定为 1（符合 VideoSubFinder），
        但状态机 detect 到帧 526（17.5s，"你开窍..."段起始）后未保存该段，
        15-24s 段全丢；相邻段（494"所以"、621"而你自己..."）正常。
      - 已定位：问题在 run_state_machine 的 track 段结束/保存逻辑，非 second_filtration。
      - 需逐块对比 VideoSubFinder FastSearchSubtitles（SSAlgorithms.cpp 1413-2200）
        与 run_state_machine（state.rs），重点：
        - bln（GetIntersectImages）与 cur_pos/prev_pos 的段边界判定
        - AnalizeImageForSubPresence 保存判定
        - finded_prev / bf / pbf 交互
      - 参考已定位的 Any-skip 修复（跳过 second_filtration 对齐模式段清理，line 2014
        起 `if (g_text_alignment != Any)`），但该修复让 has_text 正确却暴露了状态机问题。

- [ ] **second_filtration 对齐确认**（当前 Any-skip 会丢段，需确认 VideoSubFinder
      在 Any 下到底跳过哪些清理，避免误跳过）
- [ ] **sobel N-edge up_l 系数**（我们 10 vs VideoSubFinder 7），确认是否与状态机
      差异叠加

## 背景 / 已排除

- 参数与 VideoSubFinder 完全一致（mpn=50 / mnthr=0.3 / segh=3 等）
- 已试 mpn 调低 / mpned Any 跳过 / sobel up_l 系数 / Any-skip 对齐清理，
  均未完全修复（见 DESIGN.md「已知局限」）
- GetImNE / ApplyModerateThreshold 结构与 VideoSubFinder 一致
