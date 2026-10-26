#!/bin/bash
# Launch the demo harness muted and without taking focus, so automated
# screenshot runs don't interrupt the person using the machine.
# RACER_NO_ACTIVATE is read by the vendored bevy_winit (accessory activation
# policy + activateIgnoringOtherApps(false)). Prints the PID.
# Usage: scripts/demo-quiet.sh [log-file]
cd "$(dirname "$0")/.."
RACER_DEMO=1 RACER_MUTE=1 RACER_NO_ACTIVATE=1 ./target/release/rust-racer >"${1:-/dev/null}" 2>&1 &
echo $!
