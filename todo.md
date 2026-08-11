# 当前 TODO（细项，勾选式）

> 大方向与架构见 [ROADMAP.md](./ROADMAP.md)。本文件只列「接下来具体做哪几件」，
> 避免路线图被长清单拖垮、失真。勾选项保留近期已完成的 `[x]` 作为进度感，定期清理。

## 方向 1：视频字幕识别（核心，subtitle-ocr Rust）— 主体已实现，收尾中

- [x] subtitle-ocr Rust 实现：OCR 引擎 + 后处理 CLI 链
      （ocr-frames-adjust/filter-box、merge-frames、ocr-segment-adjust/filter）
- [x] 进度条化（indicatif，独立 stderr）+ 关 ORT 噪声（ort::logging=error）
      + `--out` 指定时不再向 stdout 重复打印整份 JSON
- [x] 修正 README/ROADMAP 里 subtitle-ocr「待实现」过期描述 → 已实现
- [ ] v3 识别掉字调参（扩张比例 / rec 输入高度 / 双线性），目标不丢字
      （环境变量 `OCR_EXPAND` 可覆盖扩张比例调试）
- [ ] subtitle-finder 长字幕 has_text 不稳定（段起始偏晚数秒）：
      `second_filtration` 的 `mpned` 检查 `n_ne < mpn(50)` 清空长字幕窄条带。
      已试 mpn 调低 / mpned Any 跳过 / sobel up_l 系数(10→7) 均未对齐
      VideoSubFinder 完整段行为（更碎或无效），疑差异在 has_text→段跟踪状态机
      （详见 `packages/subtitle-finder/DESIGN.md`「已知局限」）
- [ ] 补 v6 rec 预处理（当前占位 0.5/0.5，识别不准，标记实验性）
- [ ] cls 方向分类接入（已加载未使用，旋转文本待支持）
- [ ] 三实现横比基准：`tests/bench/subtitle-ocr` 的 `bin/bench.rs` 性能占位补实

## 方向 2：GUI 自动化测试

- [ ] opencv 视觉层：版面 / 图标 / 状态识别（文字层补充）
- [ ] (可选) yolo 控件检测，降低纯 OCR 误判

## 方向 3：GUI 智能操作

- [ ] ui_probe：OCR 结果 → 操作回灌最小闭环（waydroid 截图 → OCR → 断言）

## 跨仓维护

- [ ] 确认 `models/rapidocr` 权重交付方式（当前 `.gitignore`，本地需放置；
      考虑 sync 脚本 / 下载说明，避免协作者缺权重）
