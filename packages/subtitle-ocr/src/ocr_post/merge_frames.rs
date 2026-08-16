//! 多帧合并（相邻帧去重 / 子串合并）的参数。
//!
//! 对齐 LocalDub `packages/core/stages/ocr/utils.ts` 的 `MergeFramesArgsSchema`：
//! - `is_merge_substring`：是否把互为子串的相邻文本合并；省略时默认 `false`。
//! - `dedup_edit_distance`：`dedupOverlap` 的编辑距离阈值，edit_distance ≤ 此值则合并；
//!   省略时默认 `1`。

use crate::{FrameResult, SubtitleSegment};
use serde::Deserialize;
use serde::Serialize;

/// 多帧合并参数（对齐 LocalDub `MergeFramesArgsSchema`）。
///
/// 两个字段都用 `Option` 保留「可省略」语义（对齐 zod 的 `.optional()` + `.default(...)`），
/// 通过 [`MergeFramesArgs::is_merge_substring`] / [`MergeFramesArgs::dedup_edit_distance`]
/// 取值即自动补默认。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MergeFramesArgs {
    /// 是否合并互为子串的相邻文本。默认 `false`。
    #[serde(default = "default_is_merge_substring")]
    pub is_merge_substring: Option<bool>,
    /// dedupOverlap 的编辑距离阈值：edit_distance ≤ 此值则合并。默认 `1`。
    #[serde(default = "default_dedup_edit_distance")]
    pub dedup_edit_distance: Option<u32>,
}

fn default_is_merge_substring() -> Option<bool> {
    Some(false)
}

fn default_dedup_edit_distance() -> Option<u32> {
    Some(1)
}

impl MergeFramesArgs {
    /// 解析实际生效的 `is_merge_substring`：省略时为默认 `false`。
    pub fn is_merge_substring(&self) -> bool {
        self.is_merge_substring.unwrap_or(false)
    }

    /// 解析实际生效的 `dedup_edit_distance`：省略时为默认 `1`。
    pub fn dedup_edit_distance(&self) -> u32 {
        self.dedup_edit_distance.unwrap_or(1)
    }
}

/// 组成字幕段（[`OcrSegment`]）的单个帧明细（对齐 LocalDub `SegmentFrame`）。
///
/// `timestamp` 默认即毫秒（对齐 [`crate::FrameResult::timestamp`] 语义）；`text_confidence`
/// 为文本置信度（f32，与 [`crate::OcrBoxResult::text_confidence`] 一致）。
/// 多帧合并输出（对齐 LocalDub `MergeFramesResult`）。
///
/// `text` 为所有段文本按空格拼接的全文；`segments` 为合并后的字幕段时间轴。
#[derive(Clone, Debug, Serialize)]
pub struct MergeFramesResult {
    /// 全文：各段 `text` 以空格拼接。
    pub text: String,
    /// 合并后的字幕段列表。
    pub segments: Vec<OcrSegment>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SegmentFrame {
    /// 帧文本。
    pub text: String,
    /// 帧时刻（毫秒）。
    pub timestamp: u64,
    /// 文本置信度。
    pub text_confidence: f32,
}

/// 一条字幕段（对齐 LocalDub `OcrSegment`）。
///
/// TS 用 `extends SubtitleSegment` 继承 `text` / `start_ms` / `end_ms`；Rust 无类型继承，
/// 用 `#[serde(flatten)]` 内嵌 [`SubtitleSegment`]，序列化后与 TS 字段平铺一致。
/// 带 `?` 的字段（TS 可选）对应 `Option`，并以 `skip_serializing_if` 在输出时省略 `None`，
/// 与「可选字段缺省」的语义对齐。
#[derive(Clone, Debug, Serialize)]
pub struct OcrSegment {
    #[serde(flatten)]
    pub base: SubtitleSegment,
    /// 字幕带纵向值域 `[min_y, max_y]`（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_range: Option<[f32; 2]>,
    /// 字幕文本置信度（必填）。
    pub text_confidence: f32,
    /// 该段聚合的帧数（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_count: Option<u32>,
    /// 组成该段的各帧明细（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<SegmentFrame>>,
}

/// 归一化文本：去掉所有空白字符（对齐 LocalDub `utils.ts` 的 `normalize`，
/// TS 用 `s.replace(/\s+/g, "")`）。用于字幕文本的比对 / 子串合并判断，
/// 消除换行、空格、全/半角空白差异。返回新字符串，不改原输入。
pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 平均置信度：输入为空时返回 `0.0`（对齐 LocalDub `utils.ts` 的 `avgConfidence`）。
///
/// `confidences` 为各文本置信度（f32）；空切片返回 0 而非 panic / NaN。
pub fn avg_confidence(confidences: &[f32]) -> f32 {
    if confidences.is_empty() {
        0.0
    } else {
        confidences.iter().sum::<f32>() / confidences.len() as f32
    }
}

/// 判断 `a` 是否为 `b` 的子串（双向：较短者被较长者包含即算）。
///
/// 对齐 LocalDub `utils.ts` 的 `isSubstringOf`：
/// - 任一为空、或两串等长 ⇒ 返回 `false`（等长不算子串，避免自身匹配）；
/// - 否则较短串是否被较长串 `contains`。
///
/// 通常配合 [`normalize`] 先去空白再比对。
pub fn is_substring_of(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() || a.len() == b.len() {
        return false;
    }
    if a.len() < b.len() {
        b.contains(a)
    } else {
        a.contains(b)
    }
}

/// 合并两个（可选）置信度为均值；任一为 `None` 时返回另一个（再无则 `0.0`）。
///
/// 对齐 LocalDub `utils.ts` 的 `mergeConfidence`：参数均为可选 number，
/// 仅有一个有值时取该值，两者都有时取平均。
pub fn merge_confidence(a: Option<f32>, b: Option<f32>) -> f32 {
    match (a, b) {
        (Some(x), Some(y)) => (x + y) / 2.0,
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => 0.0,
    }
}

/// 编辑距离（Levenshtein）：将一个字符串变成另一个所需的最少
/// 插入 / 删除 / 替换次数（对齐 LocalDub `utils.ts` 的 `edit_distance`）。
///
/// 按**字符**遍历（而非字节），以对齐 TS `a.length`（UTF-16 码元计数，BMP 字符每字 1 个）。
/// 若按字节遍历，CJK 每字 3 字节会使距离被放大 3 倍，与 TS 结果不符。
/// 例：`edit_distance("陆", "陆执巡") == 2`（插入「执」「巡」两次）。
pub fn edit_distance(a: &str, b: &str) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    // 滚动两行 DP，避免 (m+1)*(n+1) 矩阵分配；每行长为 n+1。
    let mut prev: Vec<u32> = (0..=n as u32).collect();
    let mut cur: Vec<u32> = vec![0; n + 1];
    for i in 1..=m {
        cur[0] = i as u32;
        for j in 1..=n {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1]
            } else {
                prev[j].min(cur[j - 1]).min(prev[j - 1]) + 1
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

/// 两个值域 `[min, max]` 是否重叠（对齐 LocalDub `utils.ts` 的 `overlap`）。
///
/// 重叠条件：`a[0] < b[1] && b[0] < a[1]`；任一为 `None` 返回 `false`。
/// 用于判断相邻帧的时间 / y 区间是否相交（如去重时的区间碰撞检测）。
pub fn overlap(a: Option<[f32; 2]>, b: Option<[f32; 2]>) -> bool {
    match (a, b) {
        (Some([a0, a1]), Some([b0, b1])) => a0 < b1 && b0 < a1,
        _ => false,
    }
}

/// 把逐帧 [`FrameResult`] 合并成带时间轴的字幕段 [`OcrSegment`]（对齐 LocalDub
/// `utils.ts` 的 `base_merge_frames`）。
///
/// 状态机：空帧标记 gap → gap 内同文本恢复则合并、否则 flush；文本变更则 flush 旧段开新段；
/// 循环结束 flush 最后一段。每段聚合所有组成帧的置信度均值、记录 `frame_count` 与各帧明细。
///
/// 注：TS 版签名含 `args: MergeFramesArgs`，但函数体内并未使用（去重/子串合并的阈值逻辑
/// 尚未接入）。为保持签名一致这里仍接收 `&MergeFramesArgs`，以 `_args` 命名避免未用警告，
/// 待后续接入 `is_merge_substring` / `dedup_edit_distance` 时再启用。
///
/// 类型映射：`FrameResult.text_confidence` 为 `f64`，而 [`OcrSegment`] / [`SegmentFrame`]
/// 的置信度为 `f32`；此处按 `as f32` 收敛（置信度 ~0..1，f32 精度足够）。
pub fn base_merge_frames(
    frames: &[FrameResult],
    _args: &MergeFramesArgs,
) -> Vec<OcrSegment> {
    let mut segments: Vec<OcrSegment> = Vec::new();
    let mut current_text = String::new();
    let mut current_start: u64 = 0;
    let mut current_end: u64 = 0;
    let mut current_box_y: Option<[f32; 2]> = None;
    let mut gap_start: u64 = 0;
    let mut current_confidences: Vec<f32> = Vec::new();
    let mut current_frames: Vec<SegmentFrame> = Vec::new();

    // 把当前累积的段 flush 进 segments（消费 current_frames，随后由调用方重置）。
    let flush = |current_text: &str,
                     current_start: u64,
                     end_ms: u64,
                     current_box_y: Option<[f32; 2]>,
                     current_confidences: &[f32],
                     current_frames: Vec<SegmentFrame>,
                     segments: &mut Vec<OcrSegment>| {
        segments.push(OcrSegment {
            base: SubtitleSegment {
                text: current_text.to_string(),
                start_ms: current_start,
                end_ms,
            },
            y_range: current_box_y,
            text_confidence: avg_confidence(current_confidences),
            frame_count: Some(current_confidences.len() as u32),
            frames: Some(current_frames),
        });
    };

    for f in frames {
        // ─── A: 空帧 → 标记 gap ───
        if f.text.is_empty() {
            if !current_text.is_empty() && gap_start == 0 {
                gap_start = f.timestamp;
            }
            continue;
        }
        // ─── B: gap 恢复检查（空帧后同 text 恢复）───
        if gap_start > 0 {
            let gap_ms = f.timestamp.saturating_sub(gap_start);
            if gap_ms <= 1500
                && (normalize(&f.text) == normalize(&current_text)
                    || is_substring_of(&f.text, &current_text)
                    || is_substring_of(&current_text, &f.text))
            {
                // B1: gap 恢复成功 → 合并回当前段
                current_confidences.push(f.text_confidence as f32);
                current_end = f.timestamp;
                gap_start = 0;
                continue;
            }
            // B2: gap 恢复失败 → flush 当前段，重置
            flush(
                &current_text,
                current_start,
                gap_start,
                current_box_y,
                &current_confidences,
                std::mem::take(&mut current_frames),
                &mut segments,
            );
            current_text.clear();
            current_start = 0;
            current_box_y = None;
            gap_start = 0;
            current_confidences.clear();
            // current_frames 已被 take 走，重新置空
            current_frames = Vec::new();
        }
        // ─── C: text 比较 ───
        if current_text.is_empty() || normalize(&f.text) != normalize(&current_text) {
            // C1: 不同 text → flush 旧段，开始新段
            if !current_text.is_empty() {
                flush(
                    &current_text,
                    current_start,
                    current_end,
                    current_box_y,
                    &current_confidences,
                    std::mem::take(&mut current_frames),
                    &mut segments,
                );
            }
            current_text = f.text.clone();
            current_start = f.timestamp;
            current_end = f.timestamp;
            current_box_y = Some(f.y_range);
            current_confidences = vec![f.text_confidence as f32];
            current_frames = vec![SegmentFrame {
                timestamp: f.timestamp,
                text: f.text.clone(),
                text_confidence: f.text_confidence as f32,
            }];
        } else {
            // C2: 同 text → 延续当前段
            current_confidences.push(f.text_confidence as f32);
            current_end = f.timestamp;
            current_frames.push(SegmentFrame {
                timestamp: f.timestamp,
                text: f.text.clone(),
                text_confidence: f.text_confidence as f32,
            });
        }
    }
    // ─── D: 循环结束 flush 最后一段 ───
    if !current_text.is_empty() {
        let last_ts = if gap_start > 0 { gap_start } else { current_end };
        flush(
            &current_text,
            current_start,
            last_ts,
            current_box_y,
            &current_confidences,
            std::mem::take(&mut current_frames),
            &mut segments,
        );
    }
    segments
}

/// 把 `b` 合并进 `a` 得到新段：时间为两者 `min(start)` / `max(end)`，置信度取
/// [`merge_confidence`]，`frame_count` 相加，`frames` 拼接（a 在前、b 在后）。
/// `text` 与 `y_range` 由调用方决定（不同合并策略取较长文本 / 某侧 y 值域）。
///
/// 各合并函数（去重、子串等）的公共逻辑收口于此，避免重复字段拼接。
fn merge_two_segments(
    a: &OcrSegment,
    b: &OcrSegment,
    text: String,
    y_range: Option<[f32; 2]>,
) -> OcrSegment {
    let mut frames = a.frames.clone().unwrap_or_default();
    frames.extend(b.frames.clone().unwrap_or_default());
    OcrSegment {
        base: SubtitleSegment {
            text,
            start_ms: a.base.start_ms.min(b.base.start_ms),
            end_ms: a.base.end_ms.max(b.base.end_ms),
        },
        y_range,
        text_confidence: merge_confidence(Some(a.text_confidence), Some(b.text_confidence)),
        frame_count: Some(a.frame_count.unwrap_or(1) + b.frame_count.unwrap_or(1)),
        frames: Some(frames),
    }
}

/// second pass：合并相邻且互为子串、且 y 值域重叠的段（对齐 LocalDub `utils.ts`
/// 的 `mergeSubstringSegments`，处理 OCR 单字幻觉，如 `身` → `绝不起身`）。
///
/// 生成式（前向、用 `last_mut` 把 cur 吞进 prev）：对相邻 `(prev, cur)`，y 不重叠则跳过；
/// 若 `prev.text` 是 `cur.text` 子串 → 用 cur 的文本与 y 值域；若 `cur.text` 是 `prev.text`
/// 子串 → 保留 prev 的文本与 y 值域；置信度取 [`merge_confidence`]、`frame_count` 相加。
///
/// 合并出的段**不携带 `frames`**（TS 合并字面量未含 `frames` 键，序列化即丢弃），
/// 与 TS 丢弃组成帧明细的语义一致。段序列本就按时间有序，相邻合并即等价于 TS 的
/// 反向相邻合并（无需 `splice` / 重查）。
pub fn merge_substring_segments(segments: &[OcrSegment]) -> Vec<OcrSegment> {
    let mut out: Vec<OcrSegment> = Vec::new();
    for cur in segments {
        if let Some(prev) = out.last_mut() {
            if overlap(prev.y_range, cur.y_range)
                && (is_substring_of(&prev.base.text, &cur.base.text)
                    || is_substring_of(&cur.base.text, &prev.base.text))
            {
                // 把 cur 吞进 prev：prev⊂cur 时改用 cur 的文本/y 值域，终点顺延到 cur。
                if is_substring_of(&prev.base.text, &cur.base.text) {
                    prev.base.text = cur.base.text.clone();
                    prev.y_range = cur.y_range;
                }
                prev.base.end_ms = cur.base.end_ms;
                prev.text_confidence =
                    merge_confidence(Some(prev.text_confidence), Some(cur.text_confidence));
                prev.frame_count =
                    Some(prev.frame_count.unwrap_or(1) + cur.frame_count.unwrap_or(1));
                continue;
            }
        }
        out.push(cur.clone());
    }
    out
}

/// third pass：消除夹在两段相同真实字幕之间的短噪声段（A-B-C → A+C，处理 OCR 单字
/// 幻觉，如 `嗯发财了` → `菌` → `嗯发财了`）。
///
/// 对齐 LocalDub `utils.ts` 的 `removeTripletNoise`：对相邻三元组 `(a, b, c)`，
/// 当 `a`/`c` 文本编辑距离 ≤ 2 且 b 与 a/c 的 y 值域都重叠时，判定 b 是否为噪声：
/// 阈值随 b 的置信度缩放——低置信段更容易被判为噪声（短段上限放宽到 ~1500ms、
/// 同字幕编辑距离放宽到 3），高置信段需更极端才判（短段上限收紧到 500ms、
/// 编辑距离须为 0 即完全相同），避免误伤真实字幕。
/// 满足则把三段合并为一段（取 a 文本、a 起点、c 终点、a 的 y 值域，置信度取三者均值，
/// frame_count 相加，frames 拼接 a/b/c），删除 b、c 并从当前位置重新检查。
///
/// `text_confidence` 在本库为必填 `f32`（TS 的 `??` 过滤无意义），直接对三者求均值；
/// `b.text.length` 用 `chars().count()` 取字符数（对齐 TS `length` 与 `edit_distance` 的
/// 字符语义）。`b.end_ms - b.start_ms` 用 `saturating_sub` 防御反向时间戳。
pub fn remove_triplet_noise(segments: &[OcrSegment]) -> Vec<OcrSegment> {
    let mut out: Vec<OcrSegment> = segments.to_vec();
    let mut i = 0;
    while i + 2 < out.len() {
        // 整段克隆三元组，避免逐字段扒拉；随后构造 out[i] 时不再触碰 out 的借用。
        let a = out[i].clone();
        let b = out[i + 1].clone();
        let c = out[i + 2].clone();

        let a_conf = a.text_confidence.clamp(0.0, 1.0);
        let b_conf = b.text_confidence.clamp(0.0, 1.0);
        // 两端 a/c 视为「同句重影」的编辑距离阈值，由 a 的置信度调节（0~2）：
        // 低置信 a 放宽到 2、高置信 a 收紧到 0（须完全相同）。避免无关短句（如
        // 「啊」↔「我靠」距离 2）因固定阈值 ≤2 而被误组成「噪声三元组」。
        let max_triplet_edit = (1.0 - a_conf) * 2.0;
        let triplet_match = edit_distance(&a.base.text, &c.base.text) as f32 <= max_triplet_edit
            && overlap(a.y_range, b.y_range)
            && overlap(b.y_range, c.y_range);
        if !triplet_match {
            i += 1;
            continue;
        }
        // 高置信中间段（如真实短句「哈」conf 0.9998）绝不当噪声——即使时间短，
        // 也只可能是真实字幕，不是噪声闪烁。低置信段（< HIGH_CONF）才进一步判定。
        const HIGH_CONF: f32 = 0.8;
        if b_conf >= HIGH_CONF {
            i += 1;
            continue;
        }
        let dur_b = b.base.end_ms.saturating_sub(b.base.start_ms);
        // 置信度作为调节因子：低置信段更可能为噪声闪烁，阈值放宽；高置信段
        // 需更极端（更短 / 编辑距离更近）才判噪声，避免误伤真实字幕。
        //   - 时间：短段上限 500ms（高置信）~ 1500ms（低置信）
        //   - 编辑距离：同字幕阈值 0（高置信，须完全相同）~ 3（低置信）
        let max_short_ms = 500.0 + (1.0 - b_conf) * 1000.0;
        let is_short = dur_b as f32 <= max_short_ms;
        let max_edit = (1.0 - b_conf) * 3.0;
        let b_near_a = edit_distance(&b.base.text, &a.base.text) as f32 <= max_edit
            && (b.base.text.chars().count() as i32 - a.base.text.chars().count() as i32).abs()
                <= 2;
        let b_near_c = edit_distance(&b.base.text, &c.base.text) as f32 <= max_edit
            && (b.base.text.chars().count() as i32 - c.base.text.chars().count() as i32).abs()
                <= 2;
        let is_noise = is_short || b_near_a || b_near_c;
        if !is_noise {
            i += 1;
            continue;
        }
        // 合并 a/b/c → 留在位置 i；删除 b、c，并从当前位置重查。
        let merged_conf = avg_confidence(&[a.text_confidence, b.text_confidence, c.text_confidence]);
        let fc = a.frame_count.unwrap_or(1) + b.frame_count.unwrap_or(1) + c.frame_count.unwrap_or(1);
        let mut frames = a.frames.unwrap_or_default();
        frames.extend(b.frames.unwrap_or_default());
        frames.extend(c.frames.unwrap_or_default());
        out[i] = OcrSegment {
            base: SubtitleSegment {
                text: a.base.text,
                start_ms: a.base.start_ms,
                end_ms: c.base.end_ms,
            },
            y_range: a.y_range,
            text_confidence: merged_conf,
            frame_count: Some(fc),
            frames: Some(frames),
        };
        out.drain(i + 1..=i + 2);
        if i > 0 {
            i -= 1; // 重查当前位置（对齐 TS 的 i--）
        }
    }
    out
}

/// 去重 / 重叠合并（对齐 LocalDub `utils.ts` 的 `dedupOverlap`）。
///
/// 生成式（前向、用 `last_mut` 把 cur 与已合并的 prev 比）：若 prev 与 cur 时间上**重叠**
/// （`prev.start < cur.end && cur.start < prev.end`）或在 `TOUCH_GAP_MS`(500) 内**相接**，
/// 且文本编辑距离 ≤ `dedup_edit_distance`，则合并：取较长文本、`min(start)`/`max(end)`、
/// prev 的 y 值域，置信度取 [`merge_confidence`](crate::merge_confidence)、`frame_count` 相加、
/// `frames` 拼接。
///
/// `dedup_edit_distance` 对应 `MergeFramesArgs::dedup_edit_distance`（默认 1），由调用方传入；
/// 这是 `MergeFramesArgs` 阈值真正被消费的地方。段序列按时间有序，相邻合并即等价于
/// TS 的「任意 i<j 两两合并」——合并后 prev 即「更新后的 a」继续往后比。时间差用
/// `saturating_sub` 防御反向时间戳。
pub fn dedup_overlap(segments: &[OcrSegment], dedup_edit_distance: u32) -> Vec<OcrSegment> {
    const TOUCH_GAP_MS: u64 = 500;
    let mut out: Vec<OcrSegment> = Vec::new();
    for cur in segments {
        if let Some(prev) = out.last_mut() {
            let gap = prev
                .base
                .start_ms
                .max(cur.base.start_ms)
                .saturating_sub(prev.base.end_ms.min(cur.base.end_ms));
            let overlaps =
                prev.base.start_ms < cur.base.end_ms && cur.base.start_ms < prev.base.end_ms;
            let touching = gap <= TOUCH_GAP_MS;
            // 短词保护：两个都 ≤2 字、且互不相同（非子串）的相邻段不合并。
            // 短词（如单字「啊」↔「哈」）编辑距离天然为 1，极易误触发合并，但它们
            // 多为独立语气词/叹词，不同即不同的话。较长文本（如「abcdef」↔「abXYef」）
            // 的部分差异是同一字幕 OCR 微变，仍按编辑距离合并。
            let both_short = prev.base.text.chars().count() <= 2
                && cur.base.text.chars().count() <= 2;
            let not_same_word = prev.base.text != cur.base.text
                && !is_substring_of(&prev.base.text, &cur.base.text);
            if (overlaps || touching)
                && edit_distance(&prev.base.text, &cur.base.text) <= dedup_edit_distance
                && !(both_short && not_same_word)
            {
                // 合并进 prev：取较长文本（按字符数，对齐 TS length），保留 prev 的 y 值域。
                let text = if prev.base.text.chars().count() >= cur.base.text.chars().count() {
                    prev.base.text.clone()
                } else {
                    cur.base.text.clone()
                };
                *prev = merge_two_segments(prev, cur, text, prev.y_range);
                continue;
            }
        }
        out.push(cur.clone());
    }
    out
}

/// fourth pass：合并归一化后文本相同、且时间上不重叠、间隔不超过 `MAX_GAP_MS`(2s) 的
/// 相邻段（对齐 LocalDub `utils.ts` 的 `mergeAdjacentSameText`）。
///
/// 处理 A → 噪声 → A 这种被「长噪声段」切断、三元组噪声规则未触发的情况：同一字幕在
/// 标点/换气停顿切开了 ASR 切片后，于约 2s 内再次出现。反向遍历相邻 `(prev, cur)`：
/// 归一化文本不同则跳过；`cur.start - prev.end` 为「不重叠且间隔 ≤ 2s」才合并——延伸
/// prev 终点、置信度取 [`avg_confidence`]、`frame_count` 相加、`frames` 拼接。
///
/// 合并出的段**不携带 `frames`**（TS 合并字面量未含 `frames` 键，序列化即丢弃），与 TS
/// 丢弃组成帧明细的语义一致。段序列按时间有序，反向相邻合并即等价于 TS 的 `splice(i, 1)`
/// 把 cur 删进 prev。
pub fn merge_adjacent_same_text(segments: &[OcrSegment]) -> Vec<OcrSegment> {
    const MAX_GAP_MS: u64 = 2000;
    let mut out: Vec<OcrSegment> = segments.to_vec();
    // 反向遍历，把 cur 并入 prev（等价于 TS 的 segments[i-1] 吸收 segments[i] 后 splice 删除）。
    for i in (1..out.len()).rev() {
        let prev_norm = normalize(&out[i - 1].base.text);
        let cur_norm = normalize(&out[i].base.text);
        if prev_norm != cur_norm {
            continue;
        }
        let gap = out[i].base.start_ms.saturating_sub(out[i - 1].base.end_ms);
        if gap > MAX_GAP_MS {
            continue; // 仅在不重叠（gap>=0）且间隔 ≤ 2s 时合并
        }
        // 把 cur 吞进 prev：终点顺延、置信度均值、frame_count 相加、frames 拼接。
        out[i - 1].base.end_ms = out[i].base.end_ms;
        out[i - 1].text_confidence =
            avg_confidence(&[out[i - 1].text_confidence, out[i].text_confidence]);
        out[i - 1].frame_count =
            Some(out[i - 1].frame_count.unwrap_or(1) + out[i].frame_count.unwrap_or(1));
        out.remove(i);
    }
    out
}

/// 逐帧 [`FrameResult`] 合并成字幕段时间轴（对齐 LocalDub `utils.ts` 的 `mergeFrames`）。
///
/// 流水线（各 pass 均接收 `&[OcrSegment]` 返回新 `Vec`，故按值串联）：
/// 1. `base_merge_frames`：逐帧聚合为带时间轴的段；
/// 2. `merge_substring_segments`（仅当 `args.is_merge_substring`）：合并互为子串且 y
///    重叠的相邻段（OCR 单字幻觉，如 `身` → `绝不起身`）；
/// 3. `remove_triplet_noise`：消除夹在两段相同真实字幕间的短噪声段（A-B-C）；
/// 4. `dedup_overlap`：合并时间重叠/相接、文本近邻（`edit_distance ≤ dedup_edit_distance`）
///    的重复段（ASR 切片重叠产生的 `干嘛`/`于嘛` 类）；
/// 5. `merge_adjacent_same_text`：合并归一化后同文本、间隔 ≤ 2s 且不重叠的相邻段
///    （停顿/换气切开的同一字幕）。
///
/// 返回 [`MergeFramesResult`]：`text` 为各段文本空格拼接，`segments` 为最终段列表。
///
/// 注：TS 版 `mergeSubstringSegments(segments)` 未把返回值写回 `segments`（`segments` 是
/// `const` 后的数组，函数返回新数组但未被接收），属 TS 笔误；Rust 中严格按值串联，
/// 该 pass 实际生效。
pub fn merge_frames(frames: &[FrameResult], args: &MergeFramesArgs) -> MergeFramesResult {
    let mut segments = base_merge_frames(frames, args);

    // ─── Pass 1: substring merge ───
    if args.is_merge_substring() {
        segments = merge_substring_segments(&segments);
    }

    // ─── Pass 2: A-B-C triplet 噪声消除 ───
    segments = remove_triplet_noise(&segments);

    // ─── Pass 3: overlapping dedup ───
    segments = dedup_overlap(&segments, args.dedup_edit_distance());

    // ─── Pass 4: 同 text 相邻合并 ───
    segments = merge_adjacent_same_text(&segments);

    let text = segments
        .iter()
        .map(|s| s.base.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    MergeFramesResult { text, segments }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_merge_substring_is_false() {
        assert!(!MergeFramesArgs::default().is_merge_substring());
    }

    #[test]
    fn default_dedup_edit_distance_is_one() {
        assert_eq!(MergeFramesArgs::default().dedup_edit_distance(), 1);
    }

    #[test]
    fn explicit_fields_override_default() {
        let a = MergeFramesArgs {
            is_merge_substring: Some(true),
            dedup_edit_distance: Some(3),
        };
        assert!(a.is_merge_substring());
        assert_eq!(a.dedup_edit_distance(), 3);
    }

    #[test]
    fn deserialize_omitted_fields_use_default() {
        let a: MergeFramesArgs = serde_json::from_str("{}").unwrap();
        assert!(!a.is_merge_substring());
        assert_eq!(a.dedup_edit_distance(), 1);
    }

    #[test]
    fn deserialize_partial_fields_use_default_for_omitted() {
        let a: MergeFramesArgs = serde_json::from_str(r#"{"is_merge_substring":true}"#).unwrap();
        assert!(a.is_merge_substring());
        assert_eq!(a.dedup_edit_distance(), 1);
    }

    #[test]
    fn normalize_strips_all_whitespace() {
        assert_eq!(normalize("a b\nc\t d"), "abcd");
        // 原输入不被修改（返回新字符串）。
        let s = "x y".to_string();
        assert_eq!(normalize(&s), "xy");
        assert_eq!(s, "x y");
    }

    #[test]
    fn avg_confidence_mean_and_empty() {
        assert_eq!(avg_confidence(&[0.9, 0.8, 0.7]), 0.8);
        assert_eq!(avg_confidence(&[]), 0.0);
    }

    #[test]
    fn ocr_segment_serializes_flattened_with_optional_omitted() {
        // 验证 OcrSegment 序列化后字段平铺（extends SubtitleSegment 语义），
        // 且可选字段（y_range/frame_count/frames）为 None 时不出现在 JSON 中。
        let seg = OcrSegment {
            base: SubtitleSegment {
                text: "hello".into(),
                start_ms: 100,
                end_ms: 200,
            },
            y_range: None,
            text_confidence: 0.9,
            frame_count: None,
            frames: None,
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["text"], "hello");
        assert_eq!(v["start_ms"], 100);
        assert_eq!(v["end_ms"], 200);
        assert_eq!(v["text_confidence"], 0.9);
        assert!(v.get("y_range").is_none(), "可选字段 None 应省略");
        assert!(v.get("frame_count").is_none());
        assert!(v.get("frames").is_none());
    }

    #[test]
    fn ocr_segment_serializes_with_optional_present() {
        let seg = OcrSegment {
            base: SubtitleSegment {
                text: "hi".into(),
                start_ms: 0,
                end_ms: 500,
            },
            y_range: Some([10.0, 30.0]),
            text_confidence: 0.8,
            frame_count: Some(2),
            frames: Some(vec![SegmentFrame {
                text: "hi".into(),
                timestamp: 0,
                text_confidence: 0.8,
            }]),
        };
        let json = serde_json::to_string(&seg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["y_range"][0], 10.0);
        assert_eq!(v["frame_count"], 2);
        assert_eq!(v["frames"][0]["timestamp"], 0);
        assert_eq!(v["frames"][0]["text_confidence"], 0.8);
    }

    #[test]
    fn is_substring_of_bidirectional() {
        // 短串被长串包含 ⇒ true（双向：a 短则 b 含 a，反之 a 含 b）。
        assert!(is_substring_of("abc", "xxabcxx"));
        assert!(is_substring_of("xxabcxx", "abc"));
        // 等长 ⇒ false（避免自身匹配）。
        assert!(!is_substring_of("abc", "abc"));
        // 任一为空 ⇒ false。
        assert!(!is_substring_of("", "abc"));
        assert!(!is_substring_of("abc", ""));
        // 互不包含 ⇒ false。
        assert!(!is_substring_of("abc", "def"));
    }

    #[test]
    fn merge_confidence_combines() {
        // 两者都有 ⇒ 均值（f32 有舍入，用近似比较）。
        assert!((merge_confidence(Some(0.8), Some(0.6)) - 0.7).abs() < 1e-5);
        // 仅一个 ⇒ 取该值。
        assert_eq!(merge_confidence(Some(0.9), None), 0.9);
        assert_eq!(merge_confidence(None, Some(0.4)), 0.4);
        // 都为 None ⇒ 0.0。
        assert_eq!(merge_confidence(None, None), 0.0);
    }

    #[test]
    fn edit_distance_matches_ts_examples() {
        // 陆 → 陆执巡：插入「执」「巡」= 2 次操作。
        assert_eq!(edit_distance("陆", "陆执巡"), 2);
        // 陆 → 这其中是不是有什么误会：替换「陆」并插入其余 10 字 = 11 次操作
        // （该串实际为 11 个 BMP 字符；TS 注释里写的 9 为注释笔误）。
        assert_eq!(edit_distance("陆", "这其中是不是有什么误会"), 11);
        // 相同串距离为 0；单字符相等为 0。
        assert_eq!(edit_distance("abc", "abc"), 0);
        // 单字符替换 = 1。
        assert_eq!(edit_distance("a", "b"), 1);
    }

    #[test]
    fn overlap_detects_range_intersection() {
        // [0,10) 与 [5,15) 相交。
        assert!(overlap(Some([0.0, 10.0]), Some([5.0, 15.0])));
        // [0,10) 与 [10,20) 相邻不相交（严格小于）。
        assert!(!overlap(Some([0.0, 10.0]), Some([10.0, 20.0])));
        // 任一为 None ⇒ false。
        assert!(!overlap(None, Some([0.0, 10.0])));
    }

    /// 构造一个带文本与时刻的帧（其余字段占位）。
    fn frame(text: &str, ts: u64, conf: f64) -> FrameResult {
        FrameResult {
            text: text.into(),
            text_confidence: conf,
            boxes: vec![],
            x_range: [0.0, 10.0],
            y_range: [10.0, 30.0],
            timestamp: ts,
        }
    }

    #[test]
    fn base_merge_frames_groups_same_text_and_splits_on_change() {
        // 两帧同文本 → 合并成一段；第三帧不同文本 → 另起一段。
        let frames = vec![
            frame("你好", 0, 0.9),
            frame("你好", 500, 0.8),
            frame("世界", 1000, 0.7),
        ];
        let segs = base_merge_frames(&frames, &MergeFramesArgs::default());
        assert_eq!(segs.len(), 2, "同文本合并、异文本分段");
        assert_eq!(segs[0].base.text, "你好");
        assert_eq!(segs[0].base.start_ms, 0);
        assert_eq!(segs[0].base.end_ms, 500);
        assert_eq!(segs[0].frame_count, Some(2));
        // 置信度为两帧均值 (0.9+0.8)/2=0.85。
        assert!((segs[0].text_confidence - 0.85).abs() < 1e-5);
        assert_eq!(segs[1].base.text, "世界");
        assert_eq!(segs[1].base.start_ms, 1000);
    }

    #[test]
    fn base_merge_frames_recovers_gap_when_same_text() {
        // 中间空帧（gap）后同文本、间隔 < 1500ms → 合并回当前段（不分段）。
        let frames = vec![
            frame("字幕", 0, 0.9),
            frame("", 500, 0.0), // 空帧标记 gap
            frame("字幕", 800, 0.8),
        ];
        let segs = base_merge_frames(&frames, &MergeFramesArgs::default());
        assert_eq!(segs.len(), 1, "gap 内同文本恢复应合并为一段");
        assert_eq!(segs[0].frame_count, Some(2));
        assert_eq!(segs[0].base.end_ms, 800);
    }

    #[test]
    fn base_merge_frames_flushes_on_gap_then_different_text() {
        // 空帧后文本不同 → flush 当前段、开新段（两段）。
        let frames = vec![
            frame("甲", 0, 0.9),
            frame("", 500, 0.0),
            frame("乙", 900, 0.8), // 间隔 400ms 但文本不同
        ];
        let segs = base_merge_frames(&frames, &MergeFramesArgs::default());
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].base.text, "甲");
        assert_eq!(segs[0].base.end_ms, 500); // gap 处截断
        assert_eq!(segs[1].base.text, "乙");
    }

    #[test]
    fn base_merge_frames_empty_input_yields_no_segments() {
        let segs = base_merge_frames(&[], &MergeFramesArgs::default());
        assert!(segs.is_empty());
    }

    /// 构造一个字幕段（其余字段占位）。
    fn segment(text: &str, start: u64, end: u64, y: [f32; 2], conf: f32) -> OcrSegment {
        OcrSegment {
            base: SubtitleSegment {
                text: text.into(),
                start_ms: start,
                end_ms: end,
            },
            y_range: Some(y),
            text_confidence: conf,
            frame_count: Some(1),
            frames: None,
        }
    }

    #[test]
    fn merge_substring_segments_merges_overlapping_substring() {
        // prev「身」是 cur「绝不起身」的子串、y 重叠 → 合并为「绝不起身」，
        // 起点取 prev、终点取 cur。
        let segs = vec![
            segment("身", 0, 500, [10.0, 30.0], 0.9),
            segment("绝不起身", 500, 1000, [10.0, 30.0], 0.8),
        ];
        let merged = merge_substring_segments(&segs);
        assert_eq!(merged.len(), 1, "互为子串且 y 重叠应合并");
        assert_eq!(merged[0].base.text, "绝不起身");
        assert_eq!(merged[0].base.start_ms, 0);
        assert_eq!(merged[0].base.end_ms, 1000);
        // 置信度取均值 (0.9+0.8)/2 = 0.85。
        assert!((merged[0].text_confidence - 0.85).abs() < 1e-5);
        assert_eq!(merged[0].frame_count, Some(2));
    }

    #[test]
    fn merge_substring_segments_skips_when_y_not_overlap() {
        // 文本互为子串但 y 不重叠 → 不合并。
        let segs = vec![
            segment("身", 0, 500, [10.0, 30.0], 0.9),
            segment("绝不起身", 500, 1000, [200.0, 220.0], 0.8),
        ];
        let merged = merge_substring_segments(&segs);
        assert_eq!(merged.len(), 2, "y 不重叠应保留两段");
    }

    #[test]
    fn merge_substring_segments_skips_when_not_substring() {
        // 文本互不包含、y 重叠 → 不合并。
        let segs = vec![
            segment("你好", 0, 500, [10.0, 30.0], 0.9),
            segment("世界", 500, 1000, [10.0, 30.0], 0.8),
        ];
        let merged = merge_substring_segments(&segs);
        assert_eq!(merged.len(), 2, "非子串应保留两段");
    }

    #[test]
    fn remove_triplet_noise_folds_short_middle_segment() {
        // A=嗯发财了 / B=菌(短,150ms) / C=嗯发财了：A-C 编辑距离 1、y 全重叠、
        // B 很短 → 合并为一段「嗯发财了」，覆盖 A 起点到 C 终点。
        let segs = vec![
            segment("嗯发财了", 0, 1000, [10.0, 30.0], 0.9),
            segment("菌", 1000, 1150, [10.0, 30.0], 0.3),
            segment("嗯发财了", 1150, 2000, [10.0, 30.0], 0.85),
        ];
        let out = remove_triplet_noise(&segs);
        assert_eq!(out.len(), 1, "A-B-C 噪声应折叠为一段");
        assert_eq!(out[0].base.text, "嗯发财了");
        assert_eq!(out[0].base.start_ms, 0);
        assert_eq!(out[0].base.end_ms, 2000);
        // 三段置信度均值 (0.9+0.3+0.85)/3 ≈ 0.6833。
        assert!((out[0].text_confidence - (0.9 + 0.3 + 0.85) / 3.0).abs() < 1e-4);
        assert_eq!(out[0].frame_count, Some(3));
    }

    #[test]
    fn remove_triplet_noise_keeps_when_middle_is_real() {
        // A/C 文本差异大（编辑距离 > 2）→ 不触发合并，保留三段。
        let segs = vec![
            segment("今天天气真好", 0, 1000, [10.0, 30.0], 0.9),
            segment("菌", 1000, 1150, [10.0, 30.0], 0.3),
            segment("我们去爬山吧", 1150, 2000, [10.0, 30.0], 0.85),
        ];
        let out = remove_triplet_noise(&segs);
        assert_eq!(out.len(), 3, "A-C 文本差异大应保留三段");
    }

    #[test]
    fn remove_triplet_noise_keeps_when_no_y_overlap() {
        // A-C 文本相同但 B 与 A/C 的 y 不重叠 → 不合并。
        let segs = vec![
            segment("嗯发财了", 0, 1000, [10.0, 30.0], 0.9),
            segment("菌", 1000, 1150, [200.0, 220.0], 0.3),
            segment("嗯发财了", 1150, 2000, [10.0, 30.0], 0.85),
        ];
        let out = remove_triplet_noise(&segs);
        assert_eq!(out.len(), 3, "y 不重叠应保留三段");
    }

    #[test]
    fn remove_triplet_noise_keeps_high_conf_short_middle() {
        // 复现「好吧」/「那我就开始了」/「请问」误合并：A/C 编辑距离 2（≤2）、
        // 中间段 1000ms 卡在原 is_short 阈值，但置信度 0.95 高。
        // 置信度缩放后：max_short = 500+0.05×1000=550ms，1000 > 550 → 不短；
        // max_edit = 0.05×3=0.15，编辑距离 6 → 不近。→ 保留三段。
        let segs = vec![
            segment("好吧", 26300, 26467, [626.0, 671.0], 0.998),
            segment("那我就开始了", 27900, 28900, [629.0, 669.0], 0.95),
            segment("请问", 29700, 30300, [627.0, 670.0], 0.99),
        ];
        let out = remove_triplet_noise(&segs);
        assert_eq!(out.len(), 3, "高置信的真实中段不应被判为噪声折叠");
        assert_eq!(out[1].base.text, "那我就开始了");
    }

    #[test]
    fn remove_triplet_noise_keeps_consecutive_high_conf_short_lines() {
        // 真实回归：大/48 里「啊/哈/我靠」三句连续高置信短句。原逻辑把「哈」
        // （conf 0.9998，433ms）因 is_short 判为噪声，且 edit_distance(啊,我靠)=2 触发
        // triplet_match，把三句折叠成一段「啊」。修复后：
        //   - triplet_match 的编辑距离阈值由 a 置信度调节（0.998→≈0），啊↔我靠 距离 2 > 0 → 不组三元组；
        //   - 即便组成，b_conf=0.9998 ≥ 0.8 高置信 → 保留。
        // 故三段应各自保留。
        let segs = vec![
            segment("啊", 34933, 35400, [604.0, 644.0], 0.9402886),
            segment("哈", 35433, 35866, [603.0, 644.0], 0.9998287),
            segment("我靠", 35900, 36666, [603.0, 644.0], 0.9996991),
        ];
        let out = remove_triplet_noise(&segs);
        assert_eq!(out.len(), 3, "啊/哈/我靠 三句高置信连续短句应保留为三段");
        assert_eq!(out[0].base.text, "啊");
        assert_eq!(out[1].base.text, "哈");
        assert_eq!(out[2].base.text, "我靠");
    }

    #[test]
    fn dedup_overlap_merges_overlapping_similar_text() {
        // a/b 时间相接（gap 100ms ≤ 500）且文本编辑距离 ≤ 1 → 合并。
        let segs = vec![
            segment("嗯发财了", 0, 1000, [10.0, 30.0], 0.9),
            segment("嗯发财", 1000, 1100, [10.0, 30.0], 0.8), // 差 1 字
        ];
        let out = dedup_overlap(&segs, 1);
        assert_eq!(out.len(), 1, "重叠+近文本应合并");
        assert_eq!(out[0].base.text, "嗯发财了", "取较长文本");
        assert_eq!(out[0].base.start_ms, 0);
        assert_eq!(out[0].base.end_ms, 1100);
        assert_eq!(out[0].frame_count, Some(2));
    }

    #[test]
    fn dedup_overlap_keeps_dissimilar_text() {
        // 时间重叠但编辑距离 > 阈值 → 不合并。
        let segs = vec![
            segment("今天天气真好", 0, 1000, [10.0, 30.0], 0.9),
            segment("完全不同的字幕", 500, 1500, [10.0, 30.0], 0.8),
        ];
        let out = dedup_overlap(&segs, 1);
        assert_eq!(out.len(), 2, "编辑距离过大应保留两段");
    }

    #[test]
    fn dedup_overlap_respects_edit_distance_threshold() {
        // 文本差 2 字，阈值=1 时不合并；阈值=2 时合并。
        let segs = vec![
            segment("abcdef", 0, 1000, [10.0, 30.0], 0.9),
            segment("abXYef", 500, 1500, [10.0, 30.0], 0.8),
        ];
        assert_eq!(dedup_overlap(&segs, 1).len(), 2, "阈值 1 不合并");
        assert_eq!(dedup_overlap(&segs, 2).len(), 1, "阈值 2 合并");
    }

    #[test]
    fn dedup_overlap_keeps_short_dissimilar_high_conf() {
        // 真实回归：大/48 里「啊」↔「哈」两个单字高置信短句，时间相接（gap 33ms ≤ 500）、
        // 编辑距离 1（单字替换）≤ 默认阈值 1——若按原逻辑会合并为一段「啊」。
        // 但二者是不同语气词（非子串、均 ≤2 字），应保留为两段，不被误吞。
        let segs = vec![
            segment("啊", 34933, 35400, [604.0, 644.0], 0.9402886),
            segment("哈", 35433, 35866, [603.0, 644.0], 0.9998287),
        ];
        let out = dedup_overlap(&segs, 1);
        assert_eq!(out.len(), 2, "啊/哈 两个不同单字高置信短句不应合并");
        assert_eq!(out[0].base.text, "啊");
        assert_eq!(out[1].base.text, "哈");
    }

    #[test]
    fn merge_adjacent_same_text_merges_same_text_within_gap() {
        // A(0-1000) → 停顿 → A(2500-3500)：归一化文本相同、间隔 1500ms ≤ 2s 且不重叠
        // → 合并为一段，终点顺延到 3500，frame_count=2，置信度取均值。
        let segs = vec![
            segment("你好", 0, 1000, [10.0, 30.0], 0.9),
            segment("你好", 2500, 3500, [10.0, 30.0], 0.8),
        ];
        let out = merge_adjacent_same_text(&segs);
        assert_eq!(out.len(), 1, "归一化同文本、间隔≤2s 应合并");
        assert_eq!(out[0].base.text, "你好");
        assert_eq!(out[0].base.start_ms, 0);
        assert_eq!(out[0].base.end_ms, 3500);
        assert_eq!(out[0].frame_count, Some(2));
        assert!((out[0].text_confidence - 0.85).abs() < 1e-5);
    }

    #[test]
    fn merge_adjacent_same_text_keeps_when_gap_too_large() {
        // 文本相同但间隔 3000ms > 2s → 不合并，保留两段。
        let segs = vec![
            segment("你好", 0, 1000, [10.0, 30.0], 0.9),
            segment("你好", 4000, 5000, [10.0, 30.0], 0.8),
        ];
        let out = merge_adjacent_same_text(&segs);
        assert_eq!(out.len(), 2, "间隔>2s 应保留两段");
    }

    #[test]
    fn merge_adjacent_same_text_normalizes_whitespace() {
        // 归一化后文本相同（差异仅空白）→ 合并。
        let segs = vec![
            segment("你 好", 0, 1000, [10.0, 30.0], 0.9),
            segment("你好", 1500, 2500, [10.0, 30.0], 0.8),
        ];
        let out = merge_adjacent_same_text(&segs);
        assert_eq!(out.len(), 1, "归一化同文本应合并");
        assert_eq!(out[0].base.end_ms, 2500);
    }

    #[test]
    fn merge_frames_runs_full_pipeline() {
        // 同文本帧 → base_merge_frames 成一段；随后 pass 均不拆它。
        let frames = vec![
            frame("你好世界", 0, 0.9),
            frame("你好世界", 500, 0.8),
            frame("", 600, 0.0), // 空帧制造 gap
            frame("你好世界", 800, 0.85), // 间隔 200ms 同文本 → gap 恢复合并
        ];
        let result = merge_frames(&frames, &MergeFramesArgs::default());
        assert_eq!(result.segments.len(), 1, "同文本应合并成一段");
        assert_eq!(result.segments[0].base.text, "你好世界");
        assert_eq!(result.segments[0].base.end_ms, 800);
        // text 为各段文本空格拼接。
        assert_eq!(result.text, "你好世界");
    }

    #[test]
    fn merge_frames_substring_pass_merges_single_char_hallucination() {
        // is_merge_substring 开启时，「身」(子串) 与「绝不起身」合并为「绝不起身」。
        let frames = vec![
            frame("身", 0, 0.9),
            frame("绝不起身", 500, 0.8),
        ];
        let args = MergeFramesArgs {
            is_merge_substring: Some(true),
            dedup_edit_distance: Some(1),
        };
        let result = merge_frames(&frames, &args);
        assert_eq!(result.segments.len(), 1, "子串 pass 应合并");
        assert_eq!(result.segments[0].base.text, "绝不起身");
    }
}
