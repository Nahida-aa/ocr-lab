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
      - **决定性发现（CPP_SF_COMPARE 隔离 compare 路径）**：C++ 的 compare 路径
        SecondFiltration 在 rows 371-434 也大量清空（5060 条 nNE<mpn，nNE 0-49），
        与 Rust **完全一致** → **second_filtration 不是差异，decoder 也不是**（两边
        nNE 都 < 50）。
      - **真正的差异在 fast `compare_two_subs` 的 val3（ILA 比较）**：C++ merge 时
        fast compare 的 Im1=8003（ImIntSP ∩ ILA1 ∩ VE1）非空 → val3=1 → 判"相同"
        → 不进入 Difficult。Rust 的 fast compare 在 merge 点 `val1=true val2=true
        val3=false` → 判"changed" → 进入 Difficult → 被 second_filtration 清空。
      - **带列表对比（C++ 加 band dump）**：fn=1712 附近 fast bands，C++ `[391-426][616-676]`
        / `[404-425][616-673]`（首带 ~391-449），Rust `355-415, 612-676`（首带 ~355）。
        **带边界显著不同（偏移 ~36-50 行）** → im_res(union of ILA-masked content) 在
        行级不同 → get_lines_info 带不同 → Rust compare2 在某带遇空（cmb=0）→ val3=false。
        带差异源于 im1/im2/ila 的行级内容差异（decoder BGR→Y→ILA 微差）。
      - **影响评估**：23 段唯一结构差异，两端同文本，仅多一个重复关键帧，深挖成本高
        收益小，用户认可"够用"。
      - 假设证伪记录：get_intersect_images 短路、second_filtration、decoder(second_filtration
        层面) 均非差异；带生产层面 im_res 行级差异是最终定位。

- [ ] **新发现：Rust 漏字幕段（大/13 第一个"走吧" 11666-12465，C++ 有 Rust 无）**
      - C++ 段含 `11666-12465`（OCR="走吧"），Rust timeline 无此段。
      - **get_lines_info 已对齐**（C++ SSAlgorithms.cpp:2179 与 Rust compare.rs:18 的
        band 扫描 + 小带合并 + 近带合并逻辑一致）。
      - **根因 = 段内容中屏孤立白点**：Rust `im_res` 在 331-339 有 1-3 孤立白点
        （im1≈13、im2≈11，im∩ila 各留 1-2 点）→ get_lines_info 产生幽灵带 → compare2
        空带 cmb=0 → val3=false → bf 每帧重置 → 段无法成段。C++ ImRES 该区干净
        （只出 [623-672] 单带）。
      - 这 1-3 点来自段内容 `im_int_s`（get_intersect_images 交集 + analyse_image_flat）
        保留了 ~13 中屏背景像素（稳定 UI/图形），C++ 无。
      - **❌ 解码器色彩假设已证伪（Step0 实验）**：bt601 vs bt709 的 BGR 42.94% 像素
        差 ±8，但喂给 `get_transformed_image` 后 331-339 白点几乎一致（235 vs 225）、
        bands 完全相同（都含 282-354/360-423）。幽灵带**不是** decoder 色彩差异导致，
        OpenCV 改写不解决问题（已放弃该方案）。注意 331-339 单帧白点 235 但段内容
        (交集) 只 13——幽灵带是多帧交集/analyse_image_flat 段内容构造问题。
      - **矛盾证据（进一步深挖）**：
        - 单帧 TF 331-339：C++ `GetTransformedImage` 238、Rust bt601 235、bt709 225
          —— C++ 也产生这些白点，非 0。
        - bt601 vs bt709 坐标：220 共有、bt601 独有 15、bt709 独有 5（阈值边缘像素）。
        - **交集后 331-339（6 帧模拟）**：bt709=39、bt601=13（Rust 当前）→ bt709 反而
          更多，非更少。**decoder 色彩不是幽灵带消失原因**。
        - 矛盾：Rust bt601 交集 13 点→段不稳定；C++（bt709）交集 39 点→能成段。
          → 幽灵带不由 331-339 白点数量直接决定，而是**段内容 ImIntS 在状态机里
          的进一步构造**（非 get_intersect_images 原始交集）。待追状态机 ImIntS 赋值。
      - 跨视频复现（大/11 56600 过度切分 + 大/13 走吧缺失）。待修方向：对齐 C++ 的
        get_intersect_images 交集 / analyse_image_flat 段内容构造（为何 C++ 保留
        331-339 少于 Rust），或让 compare2/get_lines_info 容错极小孤立白点带。
      - **✅ bgr_to_yuv 改用 OpenCV cvtColor（4443773）**：Rust 浮点 BGR2YUV 的 V 通道
        与 OpenCV 整数实现差 ±1（(331,560) V=131 vs 132）→ get_im_ff 阈值边缘像素
        FF 判定不同 → im∩ILA 重叠 4 vs 0。改用 cvtColor 后 FF 对齐、get_intersect_images
        的 im∩ILA 331-339 重叠=0（同 C++）。
      - **✅ 解码改用 OpenCV VideoCapture（08b5307）**：之前 ffmpeg-next 输出 bt601 BGR，
        C++/OpenCV 是 bt709（差 ±1-8）。改用 OpenCV VideoCapture（bt709，与 C++ 后端
        一致）+ cvtColor。**消除了 331-339 幽灵带**（im∩y 重叠 4→0）。
      - **❌ 幽灵带仍未完全解决**：顶部 26-44 幽灵带仍在（b_im1=0 b_im2=1，cmb=0）。
        C++ 的 im∩y 26-44 重叠=243（有内容），但 C++ compare 的 ImFF1∩VE11 非空（无
        幽灵带）；Rust 的 im_ff1∩ve1 在 26-44 为空（**边图 im_ne 在 26-44 为 0**）。
        即剩余差异在**边缘图 im_ne**（ImproveSobelNEdge/HEdge）在顶部/中屏特定行的
        像素级差异。即使 YUV/输入/sobel 对齐，im_ne 仍有差异。多位置孤立噪声点幽灵带，
        逐像素对齐困难。大/13 走吧段、大/11 56600 仍异常。

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
