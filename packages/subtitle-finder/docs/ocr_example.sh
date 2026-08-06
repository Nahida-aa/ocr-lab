#!/usr/bin/env bash
# ocr_example.sh —— 用 subtitle-ocr 处理 subtitle-finder 产物，输出带时间轴的字幕。
#
# 流程：
#   1. 跑 subtitle-finder 得到关键帧（原始帧 + mask + keyframes.json）
#   2. 对每个关键帧的【原始帧】用 subtitle-ocr OCR
#   3. 结合 keyframes.json 的时间轴，输出 SRT 风格字幕
#
# 用法：
#   ./ocr_example.sh <video> [out_dir]
#   例：./ocr_example.sh /tmp/clip5s.mp4 /tmp/sf_out
#   （省略 out_dir 则用 subtitle-finder 默认的 out/）
#
# 说明：subtitle-finder 的关键帧已按状态机切成字幕段（每个关键帧 = 一段字幕），
#       故这里逐张 OCR + 时间轴即可，无需 subtitle-ocr 的 --merge（那是给连续帧的）。
# 注意：subtitle-ocr 对单张图 OCR 约 400-600ms，段数 × 该耗时。

set -euo pipefail

VIDEO="${1:?用法: $0 <video.mp4> [out_dir]}"
OUT="${2:-}"

cd "$(dirname "$0")"          # subtitle-finder 包目录

echo "[1/3] 跑 subtitle-finder 提取关键帧..."
if [ -n "$OUT" ]; then
    cargo run -p subtitle-finder --release -- "$VIDEO" --out "$OUT"
else
    cargo run -p subtitle-finder --release -- "$VIDEO"
    OUT="out"
fi

echo "[2/3] 对每个关键帧原始帧 OCR..."
KEYFRAMES="$OUT/keyframes.json"
if [ ! -f "$KEYFRAMES" ]; then
    echo "找不到 $KEYFRAMES，subtitle-finder 未正常输出？" >&2
    exit 1
fi

# 用 python 解析 keyframes.json + 逐张 OCR（python 里调 subtitle-ocr）
python3 - "$OUT" <<'PY'
import json, subprocess, re, sys, os
out_dir = sys.argv[1]
with open(os.path.join(out_dir, 'keyframes.json')) as f:
    kfs = json.load(f)

def ocr(img):
    # subtitle-ocr <img> --subtitle-only 输出 JSON（数组，每项含 text / segments）
    r = subprocess.run(
        ['cargo', 'run', '-p', 'subtitle-ocr', '--release', '--', img, '--subtitle-only'],
        capture_output=True, text=True)
    try:
        data = json.loads(r.stdout)
    except json.JSONDecodeError:
        return ''
    if isinstance(data, list):
        # 每项可能含 "text"；取所有非空文本拼起来
        texts = [d.get('text', '') for d in data if isinstance(d, dict) and d.get('text')]
        return ' '.join(texts)
    if isinstance(data, dict):
        return data.get('text', '')
    return ''

print("[3/3] 字幕时间轴（SRT 风格）")
print("=" * 40)
for kf in kfs:
    img = os.path.join(out_dir, kf['image'])
    text = ocr(img)
    s, e = int(kf['start_ms']), int(kf['end_ms'])
    # ms → SRT 时间戳 HH:MM:SS,mmm
    def ts(ms):
        ms = max(0, ms)
        return f"{ms//3600000:02d}:{(ms//60000)%60:02d}:{(ms//1000)%60:02d},{ms%1000:03d}"
    print(f"{ts(s)} --> {ts(e)}")
    print(f"  {text if text else '(未识别)'}")
    print()
PY
