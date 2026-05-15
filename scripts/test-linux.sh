#!/usr/bin/env bash
# Run demo-linux's assertion modes in sequence, propagate per-mode
# exit codes, and surface an aggregate PASS/FAIL summary. Mirrors
# scripts/test-mac.sh for the Linux WebKitGTK 4.1 producer.
#
# Each mode runs offscreen (the producer hosts a GtkOffscreenWindow
# and the demo binary exits as soon as the snapshot is written), so
# the script is safe to run in the background while you work.
#
# In headless environments (CI, SSH without forwarding), wrap with
# xvfb-run:
#     xvfb-run -a bash scripts/test-linux.sh
#
# Requires:
#   - cargo on PATH
#   - the system dev packages from the Linux quick-start (see README)
#   - either a running display server (Wayland or X11) or xvfb-run

set -uo pipefail

cd "$(dirname "$0")/.."

# Per-mode wall-clock cap. Producer internals already bound their own
# navigate / snapshot timeouts at a couple seconds each; this is the
# outer fence against an unexpected hang.
TIMEOUT="${TIMEOUT:-60}"

MODES=(
    --probe-only
    --snapshot-test
    --scripted
    --input-test
    --cookie-test
    --scheme-test
    --popup-test
)

echo "==> building demo-linux"
cargo build -q -p demo-linux

passed=0
failed=0
failed_modes=()

for mode in "${MODES[@]}"; do
    echo
    echo "==> $mode"
    if perl -e 'alarm shift; exec @ARGV' "$TIMEOUT" \
        cargo run -q -p demo-linux -- "$mode"; then
        echo "  -> PASS"
        passed=$((passed + 1))
    else
        rc=$?
        if [[ $rc -eq 142 ]]; then
            echo "  -> FAIL (timed out after ${TIMEOUT}s)"
        else
            echo "  -> FAIL (exit $rc)"
        fi
        failed=$((failed + 1))
        failed_modes+=("$mode")
    fi
done

echo
echo "==> summary"
echo "  passed: $passed / ${#MODES[@]}"
echo "  failed: $failed"
if [[ $failed -gt 0 ]]; then
    for m in "${failed_modes[@]}"; do
        echo "    - $m"
    done
    exit 1
fi
echo "  all PASS"
