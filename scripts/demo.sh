#!/usr/bin/env bash
# 生成两条对照 demo：中文一条、英文一条。
# 需要 .env 中的 DOUBAO_API_KEY。
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ ! -f .env ]]; then
  echo "缺少 .env：先 cp .env.example .env，并填入 DOUBAO_API_KEY" >&2
  exit 1
fi

set -a
# shellcheck disable=SC1091
source .env
set +a

mkdir -p demo
for lang in zh en; do
  fixture="fixtures/lesson-smoke-${lang}.txt"
  out="$(mktemp -d "/tmp/dictation-demo-${lang}-XXXXXX")"
  echo "=== ${lang} ==="
  cargo run --quiet -- gen "${fixture}" \
    --provider doubao \
    --out "${out}" \
    --full-track
  cp "${out}/full.mp3" "demo/smoke-${lang}.mp3"
  echo "demo: demo/smoke-${lang}.mp3"
done

ffprobe -v error \
  -show_entries format=duration,format_name \
  -show_entries stream=sample_rate,channels,codec_name \
  -of default=noprint_wrappers=1 demo/smoke-zh.mp3
ffprobe -v error \
  -show_entries format=duration,format_name \
  -show_entries stream=sample_rate,channels,codec_name \
  -of default=noprint_wrappers=1 demo/smoke-en.mp3
