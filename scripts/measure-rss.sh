#!/usr/bin/env bash
# Measure znonclip's idle memory.
#
# Reports two numbers for the running process:
#   RSS            resident set size (ps)
#   phys_footprint what Activity Monitor's "Memory" column shows (footprint / vmmap)
#
# Usage:
#   scripts/measure-rss.sh            # samples the running znonclip
#   scripts/measure-rss.sh 30 5       # 5 samples, 30 s apart
set -euo pipefail

interval="${1:-10}"
samples="${2:-3}"

pid="$(pgrep -x znonclip | head -n1 || true)"
if [[ -z "${pid}" ]]; then
  echo "znonclip is not running. Start it first: znonclip --detach" >&2
  exit 1
fi

echo "pid ${pid}; ${samples} samples, ${interval}s apart"
for ((i = 1; i <= samples; i++)); do
  rss_kb="$(ps -o rss= -p "${pid}" | tr -d ' ')"
  fp="$(footprint -p "${pid}" 2>/dev/null | grep -oE 'Footprint: [0-9.]+ [KMG]B' | head -n1 | cut -d' ' -f2- || true)"
  if [[ -z "${fp}" ]]; then
    fp="$(vmmap --summary "${pid}" 2>/dev/null | awk -F': *' '/Physical footprint:/ {print $2; exit}' || true)"
  fi
  printf '  sample %d: RSS %.1f MB   phys_footprint %s\n' "${i}" "$(echo "${rss_kb}/1024" | bc -l)" "${fp:-n/a}"
  if ((i < samples)); then sleep "${interval}"; fi
done
