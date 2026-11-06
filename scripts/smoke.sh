#!/usr/bin/env bash
# 真实 TTS 冒烟测试。用法：
#   ./scripts/smoke.sh qwen
#   ./scripts/smoke.sh doubao
# 需要先创建 .env 并填入对应凭证（不要提交 .env）。
set -euo pipefail
cd "$(dirname "$0")/.."

provider="${1:-qwen}"
if [[ ! -f .env ]]; then
  echo "缺少 .env：先 cp .env.example .env，并填入 ${provider} 的凭证" >&2
  exit 1
fi

set -a
# shellcheck disable=SC1091
source .env
set +a

out="$(mktemp -d "/tmp/dictation-smoke-${provider}-XXXXXX")"
echo "provider=${provider}"
echo "out=${out}"

cargo run --quiet -- gen fixtures/lesson-smoke.txt \
  --provider "${provider}" \
  --out "${out}" \
  --full-track

echo "--- manifest ---"
python3 - "${out}/manifest.json" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
print("title:", m["title"])
print("total_ms:", m["total_ms"])
for it in m["items"]:
    print(it["index"], it["text"], it["lang"],
          "announce=", (it.get("announce") or {}).get("duration_ms"),
          "clip=", it["clips"][0]["duration_ms"],
          "gap=", it["gap_ms"],
          "repeat=", it["repeat"])
PY

echo "--- ffprobe ---"
ffprobe -v error \
  -show_entries format=duration,format_name \
  -show_entries stream=sample_rate,channels,codec_name \
  -of default=noprint_wrappers=1 "${out}/full.mp3"

echo "试听：${out}/full.mp3"
