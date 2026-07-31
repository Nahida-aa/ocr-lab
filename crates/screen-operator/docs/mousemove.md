# 鼠标移动：ydotool 绝对移动（`mousemove -a`）在本机 KWin 下失效

## 坑

本机（KWin / Plasma 6，分数缩放 ≈1.60）下，`ydotool mousemove -a -x X -y Y`
（绝对移动模式）**完全失效**：无论传什么坐标，虚拟光标都被推到 `(1,1)` 死区，
KWin 的 `workspace.cursorPos` 读到的坐标不随之变化。

验证过程（用 KWin 脚本读 `cursorPos`）：

```bash
# 真实鼠标移动时 cursorPos 是准的（手动放右下角 → 读到 1799,0）
# 但 ydotool 绝对移动后，所有输入都读到 (1,1)：
ydotool mousemove -a -x 0    -y 0    # → KWin 读到 (1, 1)
ydotool mousemove -a -x 1440 -y 900  # → KWin 读到 (1, 1)
ydotool mousemove -a -x 2880 -y 1800 # → KWin 读到 (1, 1)
```

而**相对移动（`mousemove` 不带 `-a`）是可靠的**，单位与 KWin `cursorPos` 同为
逻辑像素：

```bash
# 当前光标 (1573,1060)，相对 +100,+100（不越界时）精确 → (1673,1160)
ydotool mousemove -x 100 -y 100   # → KWin 读到 (1673,1160)
```

> 注意：相对移动若单步越界会被 KWin clamp 到屏幕边缘（如 y 超 1125 逻辑高会被
> 夹到 1124），大距离移动应分多步。

### 影响范围

- **原地点击（`click` 作用于当前光标）正常**——它不需要移动，只发 `click` 事件。
  这就是为什么"原地点击 text.md 有效"却"定位点击 Reload 无效"：定位点击 = 移动 + 点击，
  移动那步失效，点永远落在移动前的真实光标处。
- **绝对移动失效导致"自动定位点击"整条链断掉**，且失败是静默的（ydotool 返回 0、
  看似成功，光标却没动）。

### 坐标系

- ydotool 相对移动单位 = **逻辑像素**（与 KWin `cursorPos` 一致）。
- 本机屏幕物理 2880×1800，逻辑 1800×1125，scale = 系统 160% = 1.6。
- 业务侧拿到的"物理绝对坐标"（如 ScreenCast 窗口流 493×378、KWin 几何 × scale）
  要换算成逻辑再相对移动：`逻辑 = 物理 / scale`。

## 本 crate 的处理

`screen-operator` 把「注入」与「读数」拆成两个正交 trait，再用一个**后端无关**的
闭环骨架 `Mover<I: Injector, P: Probe>` 把「想到达某逻辑坐标」变成
「读 → 差 → 移 → 确认」的反复逼近：

- [`Injector`]：发「相对移一步 / 原地按一下」的原语。桌面实现 = `YdotoolInjector`
  （`mousemove -- DX DY` + `click`），移动端将来可另写 `AdbInjector`。
- [`Probe`]：读「当前指针在哪」。桌面实现 = `KwinProbe`（包 `KdeForegrounder::cursor_pos`）。
- [`Mover`]：闭环骨架，**不认识任何后端**，只依赖上面两个 trait。桌面 =
  `Mover<YdotoolInjector, KwinProbe>`，移动端 = `Mover<AdbInjector, ScreenshotProbe>`，
  **共享同一套闭环**，分别实现接口、不共享注入代码。

`ScreenOperator` 是桌面组合层：把 `YdotoolInjector` + `KwinProbe` 拼进 `Mover`，
对外只暴露 `move_to(IVec2)` / `click_left_at(IVec2)` 这类直觉 API。所有入口
统一收 **KWin 逻辑坐标**（`glam::IVec2`），物理↔逻辑换算在「看→操作」边界做。

```rust
use screen_operator::{ScreenOperator, IVec2};
use ocr_agent::KdeForegrounder;

// 用 KdeForegrounder 组装桌面闭环：注入=ydotool，读数=fg.cursor_pos（逻辑坐标）。
let fg = KdeForegrounder::new("testing_08");
let op = ScreenOperator::new().with_foregrounder(fg);

// 入口收逻辑坐标（IVec2）；内部闭环：读当前 → 算差 → 发相对一步 → 等落盘，直至
// 偏差 ≤ 容差（不预设任何倍率，ydotool 相对移动落点不稳定，只能每步确认）。
op.move_to(IVec2::new(691, 562)).unwrap();
op.click_left_at(IVec2::new(691, 562)).unwrap();  // 闭环移动 + 原地点击
```

`move_to` / `click_at` 经 `Mover` 走相对移动闭环，绕开本机失效的 ydotool 绝对移动
（`mousemove -a`）。`click_left_at(pos)` = `click_at(pos, Left)` = 闭环移动 + 左键点击；
非左键用 `click_at(pos, btn)`。

### 读数的脏读过滤（KDE）

`KdeForegrounder::cursor_pos()` 已实现脏读过滤（连续读两次、差距 ≤ 容差才采信），
规避紧循环里 journalctl 偶发的陈旧行 race。它直接被 `KwinProbe` 复用，无需调用方
再包闭包。`cursor_pos` 返回的就是逻辑坐标，与 `move_to` 入口语义一致。

## 诊断清单（移动不生效时）

1. `ydotoold` 在跑？`systemctl --user status ydotool.service`。
2. 是绝对移动失效还是整条链失效？先测**原地点击**（`--click-current` / `click_current`）
   是否生效——生效说明 ydotool 注入 OK，问题在移动。
3. 移动不收敛 → 确认 `ScreenOperator` 已用 `with_foregrounder(fg)` 组装，且 `fg` 的
   `cursor_pos` 能返回有效坐标（KWin `cursorPos` 对真实鼠标准、对 ydotool 绝对移动
   不准，但对相对移动后准）。
4. 相对移动后仍偏 → 检查物理↔逻辑换算：目标应是逻辑坐标（KWin 逻辑宽 1800、高 1125，
   本机 scale = 系统 160% = 1.6）；若上游给的是物理像素需 ÷ scale。

## 相对移动单次可靠区实测（`move_once` 原语）

`Mover::move_to` 闭环每步调一次 `Injector::move_once`（即 ydotool `mousemove -- DX DY`）。
为确定"单次相对移动到底多可靠"，用 `examples/step_stability.rs` 做了**不同距离 × 多次**
扫描：每轮先 `move_to(起点)` 闭环回起点 → 读 `before = cursor_pos()`（KWin 真实坐标）
→ `move_once((dist, 0))` → 读 `after = cursor_pos()` → **实际位移 = `after - before`（测量值）**
→ 误差 = 实际位移 − 指令距离（派生值）。

> 注意：**实际位移是测量出来的（`before`/`after` 都是 KWin 读数相减），误差才是算出来的。**
> 不要反过来从误差推实际位移。

### 坐标系前提

- 本机屏幕**逻辑**尺寸 1800×1125，坐标从 **0 开始**，故右边缘最大 x = **1799**、下边缘最大 y = 1124。
- 相对移动单步若越过屏幕边界，会被 **KWin 按屏幕几何 clamp** 到边缘（如 x 超 1799 夹到 1799）。

### 实测结论

| 单次指令距离 | 实测行为 |
|------|------|
| **≤ 400** | 精准（误差 0；400 八轮全 0 证实） |
| 410 | 轻微过冲（+5，轮 0 还准、轮 1 起稳定 +5~6） |
| 425 | 过冲 +13 |
| 450 | 过冲 +31 |
| 500 | 过冲 +66（实际移到 566） |
| 600~900 | 非线性暴涨，实际移量逼近屏幕右边缘 |
| **≥ 950** | 实际位移**饱和在 x≈1799**（屏幕右边缘），多余增量被 KWin clamp 丢弃 |

即：**ydotool `mousemove` 单次相对增量安全上限 ≈ 400 逻辑像素**；超过即进入过冲区，
≥950 直接被 clamp 到屏幕边缘 1799。不存在"绕回"——是单向饱和/clamp。

### 缓解：`Mover` 单步上限 `step_cap`

因单次大距离不可靠，`Mover::move_to` **不会一次性发整段 `delta`**，而是把每步增量各轴
`clamp(-step_cap, step_cap)`，`step_cap` 默认 **200**（远在 400 安全线以下，留 2× 余量），
大距离自然拆成多步逐个 `move_once` + 读回逼近。可用 builder 链式覆盖：

```rust
use screen_operator::{ScreenOperator, Mover, YdotoolInjector, KwinProbe, KdeForegrounder};
let mover = Mover::new(YdotoolInjector::new("ydotool"), KwinProbe::new(KdeForegrounder::new("app")))
    .with_step_cap(150); // 可选：调小更保守，调大更快（勿超 ~400）
```

验证：原 `move_once(1200)` 会饱和到 1799（偏 599），但 `move_to((1200,562))` 经拆步后
**偏差 (0,0)**；移到右边缘 `(1799,562)` 同样 **偏差 (0,0)**，每步严格 +200。

### 闭环步数 / 容差对比实测（`move_probe` 实测）

`step_cap` 是「速度 vs 精度」旋钮：调大省步但单步过冲、靠闭环擦屁股；调小更稳。
`tolerance` 是「到达判定阈值」：调小（如 0）要求严格归零（多耗几步纠抖动），
调大（如 2，默认）允许 ≤2 偏差即停（更快、残差 ±1~2 在像素级可忽略）。

**对比 A：左下角 (0,1124) → 中心 (900,562)，`tolerance=2`，不同 `step_cap`**

| step_cap | 步数 | 过冲情况 | 最终偏差 |
|----------|------|---------|---------|
| 200（默认） | 5 | 无（每步精准） | (0,0) |
| 300 | 4 | 轻微（iter2 到 650 应 650） | (0,0) |
| 400 | 3 | iter0 到 412(+12)、iter2 到 920(+20) | (-1, 2) |
| 500 | 4 | iter0 到 537(+37)、iter2 到 1004(+4) | (-1, 0) |
| 1000 | 3 | 发 (900,-562) 实际到 (1153,405)（x+251、y+157 大过冲）→ 回退振荡 | (2, -1) |

→ 默认 200 零过冲零残差；300 是「提速且仍精准」的甜点（省 1 步、双向仍 (0,0)）；
≥400 过冲明显、留 ±1~2 残差，对角移动单步过冲可达 +251，**不建议**。

**对比 B：同距离 (0,1124)→(900,562)，`tolerance=0`（必须严格归零）vs `tolerance=2`**

| step_cap | tol=2 步数 | tol=0 步数 | tol=0 最终偏差 |
|----------|-----------|-----------|---------------|
| 200 | 5 | 5 | (0,0) |
| 300 | 4 | 5 | (0,0) |
| 400 | 3 | 5 | (0,0) |
| 1000 | 3 | 5 | (0,0) |

→ `tolerance=0` 把所有配置都拉回完美 (0,0)（消除 tol=2 下 ≥400 的 ±1~2 残差），
但代价是大 `step_cap` 的过冲被逼着回退、**省下的步数又耗在纠偏上**（步数回到 5，
与 cap=200 相同）。结论：**tol=0 下大 cap 既没省步又引入过冲振荡，纯劣势**；
只有 cap=200 + tol=0 是「零过冲且严格归零」最干净组合。

实战建议：默认 `step_cap=200` + `tolerance=2` 够用；若业务要求严格归零（如点按钮
必须精准），用 `tolerance=0` 但**保持 cap=200**。

### 原始数据

`examples/step_stability.rs --format json --out <文件>` 产出每轮原始记录
（`dist` / `round` / `before` / `after` / `err`），已留存于本目录：

- `step_stability_960_1000.json`：960/970/980/990/1000（边界饱和区）
- `step_stability_1000_1100.json`：1000/1100（绕回区验证，结论为单向饱和非绕回）
- `step_stability_375_450.json`：375/400/425/450（安全边界定位，**400 零误差**）
- `step_stability_410_420.json`：410/420（精确边界：400 准、410 起偏）

复现：`cargo run -p screen-operator --example step_stability -- --dists 1,2,3,... --format json --out docs/step_stability.csv`

## 参考

- ydotool `mousemove` 帮助明确提示：`You need to disable mouse speed acceleration
  for correct absolute movement.` —— 本机即使如此，绝对模式仍失效，故改相对模式。
- 绝对模式死区 `(1,1)` 是 uinput 虚拟设备 `ABS_X/ABS_Y` 轴范围未正确初始化所致，
  相对事件（`REL_X/REL_Y`）不受影响。
