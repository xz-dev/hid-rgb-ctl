#!/bin/bash
# Rainbow wave (horizontal) for per-key RGB keyboards using hid-rgb-ctl.
# A rainbow gradient flows across the keyboard from left to right.
# Each lamp gets a hue based on its X position; the wave shifts over time.
#
# Requires: hid-rgb-ctl with LampArray support (per-key RGB keyboard).
# Usage: ./rainbow_wave.sh [-p /dev/hidrawN] [-s STEP] [-d DELAY]
# Ctrl+C to stop.

set -euo pipefail

# --- Configurable parameters ---
STEP=3          # hue offset increment per frame (1-10, lower = smoother)
DELAY=0.03      # seconds between frames
DEVICE_ARG=""   # optional: -p /dev/hidrawN

# --- Parse script arguments ---
while [[ $# -gt 0 ]]; do
    case "$1" in
        -p|--path)  DEVICE_ARG="-p $2"; shift 2 ;;
        -s|--step)  STEP="$2"; shift 2 ;;
        -d|--delay) DELAY="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 [-p /dev/hidrawN] [-s STEP] [-d DELAY]"
            echo "  -p PATH   hidraw device path (auto-detect if omitted)"
            echo "  -s STEP   hue increment per frame (default: $STEP)"
            echo "  -d DELAY  seconds between frames (default: $DELAY)"
            exit 0 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# --- HSV to RGB (H: 0-359, S/V fixed at max) ---
# Output: hex string RRGGBB
hsv2hex() {
    local h=$1
    local region=$((h / 60))
    local remainder=$(( (h % 60) * 255 / 60 ))
    local q=$((255 - remainder))
    local t=$remainder
    local r g b

    case $region in
        0) r=255; g=$t;   b=0   ;;
        1) r=$q;  g=255;  b=0   ;;
        2) r=0;   g=255;  b=$t  ;;
        3) r=0;   g=$q;   b=255 ;;
        4) r=$t;  g=0;    b=255 ;;
        *) r=255; g=0;    b=$q  ;;
    esac

    printf '%02x%02x%02x' "$r" "$g" "$b"
}

# --- Parse lamp positions from hid-rgb-ctl get ---
echo "Querying device info..."
# shellcheck disable=SC2086
GET_OUTPUT="$(hid-rgb-ctl $DEVICE_ARG get 2>&1)" || {
    echo "Error: Failed to query device. Is the device connected and accessible?"
    echo "$GET_OUTPUT"
    exit 1
}

# Extract lamp count
LAMP_COUNT=$(echo "$GET_OUTPUT" | grep -oP 'Lamps:\s+\K[0-9]+')
if [[ -z "$LAMP_COUNT" || "$LAMP_COUNT" -lt 2 ]]; then
    echo "Error: Need at least 2 lamps for wave effect (found: ${LAMP_COUNT:-0})."
    echo "This script requires a per-key RGB keyboard."
    exit 1
fi

echo "Found $LAMP_COUNT lamps. Parsing positions..."

# Extract per-lamp X positions (in mm, as integers for bash math)
# Format from hid-rgb-ctl get:
#   Lamp 0:
#     Position: (12.3, 45.6, 0.0) mm
declare -a LAMP_IDS
declare -a LAMP_X

current_id=""
while IFS= read -r line; do
    if [[ "$line" =~ ^Lamp\ ([0-9]+): ]]; then
        current_id="${BASH_REMATCH[1]}"
    elif [[ -n "$current_id" && "$line" =~ Position:\ \(([0-9.]+),\ ([0-9.]+) ]]; then
        # Store X as integer (tenths of mm) for bash integer math
        x_mm="${BASH_REMATCH[1]}"
        # Convert float to integer tenths: "12.3" -> 123, "45.6" -> 456
        x_int=$(echo "$x_mm" | awk '{printf "%d", $1 * 10}')
        LAMP_IDS+=("$current_id")
        LAMP_X+=("$x_int")
        current_id=""
    fi
done <<< "$GET_OUTPUT"

n=${#LAMP_IDS[@]}
if [[ $n -lt 2 ]]; then
    echo "Error: Could not parse lamp positions (parsed $n lamps)."
    exit 1
fi

# Find min/max X for normalization
x_min=${LAMP_X[0]}
x_max=${LAMP_X[0]}
for x in "${LAMP_X[@]}"; do
    (( x < x_min )) && x_min=$x
    (( x > x_max )) && x_max=$x
done
x_range=$((x_max - x_min))
if [[ $x_range -eq 0 ]]; then
    x_range=1  # avoid division by zero
fi

echo "X range: ${x_min}..${x_max} (tenths of mm), $n lamps."

# Pre-compute normalized X positions scaled to 0-359 (hue range)
declare -a LAMP_HUE_BASE
for i in $(seq 0 $((n - 1))); do
    # hue_base = (x - x_min) * 359 / x_range
    LAMP_HUE_BASE[i]=$(( (LAMP_X[i] - x_min) * 359 / x_range ))
done

# --- Cleanup on exit ---
cleanup() {
    echo ""
    echo "Stopped. Restoring autonomous mode..."
    # shellcheck disable=SC2086
    hid-rgb-ctl $DEVICE_ARG auto on 2>/dev/null || true
    exit 0
}
trap cleanup INT TERM

# --- Main animation loop ---
echo "Rainbow wave running (step=$STEP, delay=$DELAY, lamps=$n). Ctrl+C to stop."

hue_offset=0
while true; do
    # Build set-lamp arguments: "0:RRGGBB 1:RRGGBB ..."
    cmd_args=""
    for i in $(seq 0 $((n - 1))); do
        hue=$(( (LAMP_HUE_BASE[i] + hue_offset) % 360 ))
        hex=$(hsv2hex "$hue")
        cmd_args+="${LAMP_IDS[$i]}:${hex} "
    done

    # shellcheck disable=SC2086
    hid-rgb-ctl $DEVICE_ARG set-lamp $cmd_args 2>/dev/null

    hue_offset=$(( (hue_offset + STEP) % 360 ))
    sleep "$DELAY"
done
