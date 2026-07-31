//! 闭环移动器：把「想到达某逻辑坐标」的意图变成「读→差→移→确认」的反复逼近。
//!
//! **本模块不认识 ydotool / KWin / adb 任何一个具体后端**——它只依赖两个抽象接口：
//! - [`Injector`]：发「相对移一步 / 当前点一下」
//! - [`Probe`]：读「当前指针在哪」
//!
//! 桌面 = `Mover<YdotoolInjector, KwinProbe>`；移动端 = `Mover<AdbInjector,
//! ScreenshotProbe>`。两套**共享同一个闭环骨架**，各自实现接口、不共享注入代码——
//! 这就是「桌面与移动端分别抽象」在代码里的落点：分别实现接口，共用骨架。
//!
//! 闭环之所以必要：ydotool 相对移动落点不稳定（单步量不固定、大指令过冲甚至撞墙），
//! 无法用固定倍率描述，只能每步读回确认。移动端若用截图确认，也是同一种「逼近」
//! 模式，只是 `Probe` 换成截图回看。

use std::time::Duration;

use anyhow::Result;
use glam::IVec2;
use tracing::warn;

use crate::injector::Injector;
use crate::probe::Probe;

/// 闭环移动器（泛型，编译期确定后端）。
#[derive(Clone)]
pub struct Mover<I: Injector, P: Probe> {
    inj: I,
    probe: P,
    /// 单步相对移动上限（逻辑像素/轴）。`move_to` 把大距离拆成 ≤ 此值的子步逐个
    /// `move_once` + 读回，避免单次 `move_once` 触发 ydotool 的不可靠区（实测单次
    /// 增量 ≥~425 开始过冲、≥~950 被 KWin clamp 到屏幕边缘 1799）。默认 200，远在
    /// 400 实测安全线以下，留足余量。
    step_cap: i32,
    /// 到达容差（逻辑像素/轴）。`move_to` 判定「已到达」的偏差阈值：当前与目标各轴
    /// 差的绝对值均 ≤ `tolerance` 即停止。默认 2（读 `cursor_pos` 有 ±1 脏读抖动，2 足够
    /// 吸收且不会过早停在大偏差处）。
    tolerance: i32,
}

impl<I: Injector, P: Probe> Mover<I, P> {
    /// 构造（必填：注入器 + 读数器）。`step_cap` 默认 200、`tolerance` 默认 2，可用
    /// [`with_step_cap`] / [`with_tolerance`] 链式覆盖。
    pub fn new(inj: I, probe: P) -> Self {
        Self {
            inj,
            probe,
            step_cap: 200,
            tolerance: 2,
        }
    }

    /// 链式覆盖单步上限（默认 200）。消费 self、返回新 `Mover`，便于 builder 风格：
    /// `Mover::new(inj, probe).with_step_cap(150)`。
    pub fn with_step_cap(mut self, cap: i32) -> Self {
        self.step_cap = cap;
        self
    }

    /// 链式覆盖到达容差（默认 2）。消费 self、返回新 `Mover`：
    /// `Mover::new(inj, probe).with_tolerance(1)` 要求更精准才停（但可能多耗几步
    /// 纠脏读抖动），`with_tolerance(4)` 更早停（更快但落点可能偏几像素）。
    pub fn with_tolerance(mut self, tolerance: i32) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// 原语：相对当前位置移一步 `delta`（逻辑像素），**不读回确认、不闭环**。
    ///
    /// 这是 [`Injector::move_once`] 的透传，供外部在「已知相对增量、自行负责确认」
    /// 的场景下直接调用（如验证注入本身、或实现自己的逼近策略）。绝大多数业务应
    /// 走 [`move_to`]（内部反复调用本方法并每步读 `Probe` 确认落点），不要绕开闭环
    /// 单独用——ydotool 相对移动落点不稳定，单次 `move_once` 不保证到达预期位置。
    pub fn move_once(&self, delta: IVec2) -> Result<()> {
        self.inj.move_once(delta)
    }

    /// 确保移动到逻辑坐标 `target`（不点击）。
    ///
    /// 反复「读当前 → 算差值 → 发相对一步 → 等落盘」直至偏差 ≤ `tolerance` 或达 `MAX_ITER`。
    /// 靠每步确认收敛，不预设任何倍率常数。
    pub fn move_to(&self, target: IVec2) -> Result<()> {
        const MAX_ITER: usize = 80;
        const READ_RETRY: usize = 3; // 单次读光标失败时的重试次数。
        const SETTLE: Duration = Duration::from_millis(110); // 等光标真正移动 + 日志落盘。
        let tolerance = self.tolerance; // 到达容差（可链式配置）。

        for iter in 0..MAX_ITER {
            // 读当前位置：读不到时在本轮内重试；一直失败才放弃（不静默停在原地）。
            let mut cur: Option<IVec2> = None;
            for _ in 0..READ_RETRY {
                if let Some(p) = self.probe.pointer_pos()? {
                    cur = Some(p);
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            let curr = match cur {
                Some(p) => p,
                None => {
                    warn!(target: "screen_operator::move", iter, "move_to 读位置连续失败，放弃本轮");
                    return Ok(());
                }
            };

            let delta = target - curr;
            if delta.x.abs() <= tolerance && delta.y.abs() <= tolerance {
                tracing::debug!(target: "screen_operator::move", iter, cur = %format!("({curr})"), target = %format!("({target})"), "move_to 已到达");
                return Ok(());
            }
            // 拆步：单步增量各轴夹到 [-step_cap, step_cap]，避免单次 move_once 触发
            // ydotool 不可靠区（≥~425 过冲、≥~950 被 clamp 到屏幕边缘）。下一轮循环
            // 会重新读位置并继续逼近剩余差值，自然把大距离拆成多步。
            let step = IVec2::new(
                delta.x.clamp(-self.step_cap, self.step_cap),
                delta.y.clamp(-self.step_cap, self.step_cap),
            );
            tracing::debug!(target: "screen_operator::move", iter, cur = %format!("({curr})"), target = %format!("({target})"), cmd = %format!("({step})"), "move_to 发单步");
            self.inj.move_once(step)?;
            std::thread::sleep(SETTLE);
        }
        Ok(())
    }

    /// 移动到 `target` 并在该处单击（左键）。
    pub fn click_at(&self, target: IVec2) -> Result<()> {
        self.move_to(target)?;
        self.inj.click()
    }
}
