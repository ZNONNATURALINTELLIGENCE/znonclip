#!/usr/bin/env bash
# Uninstall ZnonClip cleanly from macOS.
#
# Default (keeps your clipboard history):
#   1. quits the running app (znonclip, plus the old clip-assistant / clippin names)
#   2. removes launch-at-login: boots out and deletes the LaunchAgent for the
#      current label (com.znonclip.app) and both legacy labels
#      (com.clipassistant.app, com.clippin.app)
#   3. removes installed binaries: `cargo install` copies and /Applications/ZnonClip.app
#
# --purge also deletes history, logs and preferences:
#   ~/Library/Application Support/com.znonclip.app      (app database)
#   ~/Library/Application Support/com.znonclip.ZnonClip (CLI / agent-tools store)
#   ~/Library/Logs/ZnonClip, ~/Library/Logs/ClipAssistant
#   ~/Library/Preferences/{znonclip,znonclip-new,clip-assistant}.plist
#
# Usage:
#   scripts/uninstall.sh              # remove app + login item, keep data
#   scripts/uninstall.sh --dry-run    # print what would happen, change nothing
#   scripts/uninstall.sh --purge      # also delete data (asks first)
#   scripts/uninstall.sh --purge -y   # also delete data, no prompt
#   scripts/uninstall.sh --keep-binary  # only stop the app and remove login items
#
# Launch-at-login from a bundled .app uses SMAppService (see
# crates/znonclip/src/launch.rs). Only the app itself can unregister that, so
# for a bundled install turn "Launch at login" off in the app before running
# this; otherwise the script tells you where to remove a stale entry.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "uninstall.sh: macOS only" >&2
  exit 1
fi

dry_run=0
purge=0
assume_yes=0
keep_binary=0

for arg in "$@"; do
  case "${arg}" in
    -n | --dry-run) dry_run=1 ;;
    --purge) purge=1 ;;
    -y | --yes) assume_yes=1 ;;
    --keep-binary) keep_binary=1 ;;
    -h | --help)
      sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0"
      exit 0
      ;;
    *)
      echo "uninstall.sh: unknown option: ${arg} (try --help)" >&2
      exit 2
      ;;
  esac
done

: "${HOME:?HOME is not set}"

current_label="com.znonclip.app"
labels=("${current_label}" "com.clipassistant.app" "com.clippin.app")
process_names=("znonclip" "clip-assistant" "clippin")
gui_domain="gui/$(id -u)"
cargo_bin="${CARGO_HOME:-${HOME}/.cargo}/bin"
app_bundle="/Applications/ZnonClip.app"

changed=0

say() { printf '%s\n' "$*"; }

# Run a command, or print it under --dry-run.
run() {
  if ((dry_run)); then
    say "  would run: $*"
  else
    "$@"
  fi
}

remove_path() {
  local p="$1"
  # Only ever delete absolute paths under $HOME or the one known app bundle.
  case "${p}" in
    "${HOME}"/?* | "${app_bundle}") ;;
    *)
      echo "uninstall.sh: refusing to remove unexpected path: ${p}" >&2
      return 1
      ;;
  esac
  if [[ -e "${p}" || -L "${p}" ]]; then
    if ((dry_run)); then
      say "  would remove: ${p}"
    else
      rm -rf -- "${p}"
      say "  removed: ${p}"
    fi
    changed=1
  fi
}

# ── 1. quit the app ─────────────────────────────────────────────────────────
say "1/3 quitting ZnonClip"
for name in "${process_names[@]}"; do
  pids="$(pgrep -x "${name}" || true)"
  [[ -z "${pids}" ]] && continue
  changed=1
  if ((dry_run)); then
    say "  would stop ${name} (pid ${pids//$'\n'/ })"
    continue
  fi
  # shellcheck disable=SC2086  # word-splitting the pid list is intended
  kill -TERM ${pids} 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -x "${name}" >/dev/null || break
    sleep 0.5
  done
  if pgrep -x "${name}" >/dev/null; then
    pkill -KILL -x "${name}" 2>/dev/null || true
    say "  force-stopped ${name}"
  else
    say "  stopped ${name}"
  fi
done

# ── 2. launch at login ──────────────────────────────────────────────────────
say "2/3 removing launch at login"
for label in "${labels[@]}"; do
  plist="${HOME}/Library/LaunchAgents/${label}.plist"
  if launchctl print "${gui_domain}/${label}" >/dev/null 2>&1; then
    changed=1
    if ((dry_run)); then
      say "  would boot out ${gui_domain}/${label}"
    else
      launchctl bootout "${gui_domain}/${label}" 2>/dev/null \
        || launchctl bootout "${gui_domain}" "${plist}" 2>/dev/null \
        || true
      say "  booted out ${gui_domain}/${label}"
    fi
  fi
  remove_path "${plist}"
done

# ── 3. binaries ─────────────────────────────────────────────────────────────
if ((keep_binary)); then
  say "3/3 keeping binaries (--keep-binary)"
else
  say "3/3 removing binaries"
  if [[ -x "${cargo_bin}/znonclip" ]] && command -v cargo >/dev/null 2>&1 \
    && cargo install --list 2>/dev/null | grep -q '^znonclip '; then
    changed=1
    run cargo uninstall znonclip
    ((dry_run)) || [[ ! -e "${cargo_bin}/znonclip" ]] || remove_path "${cargo_bin}/znonclip"
  else
    remove_path "${cargo_bin}/znonclip"
  fi
  for bin in znonclip-cli znonclip-agent clip-assistant clippin; do
    remove_path "${cargo_bin}/${bin}"
  done
  remove_path "${app_bundle}"
fi

# ── optional: data ──────────────────────────────────────────────────────────
data_paths=(
  "${HOME}/Library/Application Support/com.znonclip.app"
  "${HOME}/Library/Application Support/com.znonclip.ZnonClip"
  "${HOME}/Library/Logs/ZnonClip"
  "${HOME}/Library/Logs/ClipAssistant"
  "${HOME}/Library/Preferences/znonclip.plist"
  "${HOME}/Library/Preferences/znonclip-new.plist"
  "${HOME}/Library/Preferences/clip-assistant.plist"
)

if ((purge)); then
  if ((!dry_run && !assume_yes)); then
    printf 'Delete ZnonClip clipboard history, logs and preferences? [y/N] '
    read -r reply
    [[ "${reply}" =~ ^[Yy]$ ]] || { say "kept data"; purge=0; }
  fi
fi
if ((purge)); then
  say "purging data"
  for p in "${data_paths[@]}"; do
    remove_path "${p}"
  done
else
  kept=()
  for p in "${data_paths[@]}"; do
    [[ -e "${p}" ]] && kept+=("${p}")
  done
  if ((${#kept[@]})); then
    say "kept data (run with --purge to delete):"
    printf '  %s\n' "${kept[@]}"
  fi
fi

if ((dry_run)); then
  say "dry run: nothing changed"
elif ((changed)); then
  say "ZnonClip uninstalled."
else
  say "nothing to uninstall."
fi
say "If ZnonClip still shows in System Settings > General > Login Items, remove it there."
