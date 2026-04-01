#!/bin/bash
# Rainbow meteor scan for per-key RGB keyboards using hid-rgb-ctl.
# Multiple meteors (one per row) sweep left-to-right across a white background.
# Each row's meteor has a different rainbow color. Meteors leave a fading
# rainbow trail that blends back into white.
#
# Requires: hid-rgb-ctl with LampArray support (per-key RGB keyboard).
# Usage: ./meteor_scan.sh [-p /dev/hidrawN] [-t TAIL] [-d DELAY]
# Ctrl+C to stop.

set -euo pipefail

# --- Configurable parameters ---
TAIL_LENGTH=6     # number of trailing lamps in the fade tail
DELAY=0.04        # seconds between frames
STAGGER=3         # offset between rows (lamps) so they don't all sync
DEVICE_ARG=""     # optional: -p /dev/hidrawN

# --- Parse script arguments ---
while [[ $# -gt 0 ]]; do
    case "$1" in
        -p|--path)    DEVICE_ARG="-p $2"; shift 2 ;;
        -t|--tail)    TAIL_LENGTH="$2"; shift 2 ;;
        -d|--delay)   DELAY="$2"; shift 2 ;;
        -g|--stagger) STAGGER="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 [-p /dev/hidrawN] [-t TAIL] [-d DELAY] [-g STAGGER]"
            echo "  -p PATH     hidraw device path (auto-detect if omitted)"
            echo "  -t TAIL     tail length in lamps (default: $TAIL_LENGTH)"
            echo "  -d DELAY    seconds between frames (default: $DELAY)"
            echo "  -g STAGGER  row offset in steps (default: $STAGGER)"
            exit 0 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# --- HSV to RGB (H: 0-359, S/V fixed at max) ---
# Sets global variables _r _g _b
hsv2rgb() {
    local h=$1
    local region=$((h / 60))
    local remainder=$(( (h % 60) * 255 / 60 ))
    local q=$((255 - remainder))
    local t=$remainder

    case $region in
        0) _r=255; _g=$t;   _b=0   ;;
        1) _r=$q;  _g=255;  _b=0   ;;
        2) _r=0;   _g=255;  _b=$t  ;;
        3) _r=0;   _g=$q;   _b=255 ;;
        4) _r=$t;  _g=0;    _b=255 ;;
        *) _r=255; _g=0;    _b=$q  ;;
    esac
}

# --- Lerp color channel: blend from color to white (255) ---
# lerp(color_val, factor_num, factor_den)
# factor 0/den = pure color, factor den/den = pure white
lerp_to_white() {
    local c=$1 num=$2 den=$3
    # result = c + (255 - c) * num / den
    echo $(( c + (255 - c) * num / den ))
}

# --- Parse lamp positions from hid-rgb-ctl get ---
echo "Querying device info..."
# shellcheck disable=SC2086
GET_OUTPUT="$(hid-rgb-ctl $DEVICE_ARG get 2>&1)" || {
    echo "Error: Failed to query device. Is the device connected and accessible?"
    echo "$GET_OUTPUT"
    exit 1
}

LAMP_COUNT=$(echo "$GET_OUTPUT" | grep -oP 'Lamps:\s+\K[0-9]+')
if [[ -z "$LAMP_COUNT" || "$LAMP_COUNT" -lt 2 ]]; then
    echo "Error: Need at least 2 lamps for meteor effect (found: ${LAMP_COUNT:-0})."
    exit 1
fi

echo "Found $LAMP_COUNT lamps. Parsing positions..."

# Parse lamp IDs and positions (X, Y in tenths of mm as integers)
declare -a ALL_IDS ALL_X ALL_Y
current_id=""
while IFS= read -r line; do
    if [[ "$line" =~ ^Lamp\ ([0-9]+): ]]; then
        current_id="${BASH_REMATCH[1]}"
    elif [[ -n "$current_id" && "$line" =~ Position:\ \(([0-9.]+),\ ([0-9.]+) ]]; then
        x_mm="${BASH_REMATCH[1]}"
        y_mm="${BASH_REMATCH[2]}"
        ALL_IDS+=("$current_id")
        ALL_X+=("$(echo "$x_mm" | awk '{printf "%d", $1 * 10}')")
        ALL_Y+=("$(echo "$y_mm" | awk '{printf "%d", $1 * 10}')")
        current_id=""
    fi
done <<< "$GET_OUTPUT"

n=${#ALL_IDS[@]}
if [[ $n -lt 2 ]]; then
    echo "Error: Could not parse lamp positions (parsed $n lamps)."
    exit 1
fi

# --- Group lamps into rows by Y coordinate ---
# Lamps with Y values within ROW_THRESHOLD (tenths of mm) are the same row.
ROW_THRESHOLD=30  # 3.0mm tolerance for same row

# Collect unique Y values (sorted)
declare -a unique_ys
for y in "${ALL_Y[@]}"; do
    unique_ys+=("$y")
done
# Sort and deduplicate with threshold-based grouping
mapfile -t sorted_ys < <(printf '%s\n' "${unique_ys[@]}" | sort -n)

declare -a row_centers  # representative Y for each row
for y in "${sorted_ys[@]}"; do
    matched=0
    for rc_idx in "${!row_centers[@]}"; do
        rc="${row_centers[$rc_idx]}"
        diff=$((y - rc))
        [[ $diff -lt 0 ]] && diff=$((-diff))
        if [[ $diff -le $ROW_THRESHOLD ]]; then
            matched=1
            break
        fi
    done
    if [[ $matched -eq 0 ]]; then
        row_centers+=("$y")
    fi
done

num_rows=${#row_centers[@]}
echo "Detected $num_rows rows."

# Assign each lamp to a row, then sort lamps within each row by X
# ROW_LAMP_IDS[row] = space-separated list of lamp IDs sorted by X
# ROW_LAMP_COUNT[row] = number of lamps in that row
declare -a ROW_LAMP_IDS ROW_LAMP_COUNT

for row_idx in $(seq 0 $((num_rows - 1))); do
    rc="${row_centers[$row_idx]}"
    # Collect (x, lamp_id) pairs for this row
    pairs=""
    count=0
    for i in $(seq 0 $((n - 1))); do
        diff=$((ALL_Y[i] - rc))
        [[ $diff -lt 0 ]] && diff=$((-diff))
        if [[ $diff -le $ROW_THRESHOLD ]]; then
            pairs+="${ALL_X[$i]} ${ALL_IDS[$i]}\n"
            ((count++))
        fi
    done
    # Sort by X coordinate, extract lamp IDs
    sorted_ids=$(echo -e "$pairs" | sort -n | awk '{print $2}' | tr '\n' ' ')
    ROW_LAMP_IDS[row_idx]="$sorted_ids"
    ROW_LAMP_COUNT[row_idx]=$count
done

# Print row summary
for row_idx in $(seq 0 $((num_rows - 1))); do
    echo "  Row $row_idx: ${ROW_LAMP_COUNT[$row_idx]} lamps"
done

# Find max row width for position wrapping
max_row_len=0
for row_idx in $(seq 0 $((num_rows - 1))); do
    len=${ROW_LAMP_COUNT[$row_idx]}
    (( len > max_row_len )) && max_row_len=$len
done

# Total animation length = row width + tail (so tail fully exits before restart)
anim_length=$((max_row_len + TAIL_LENGTH + 1))

# Pre-compute each row's rainbow hue (evenly distributed across 360 degrees)
declare -a ROW_HUE
for row_idx in $(seq 0 $((num_rows - 1))); do
    ROW_HUE[row_idx]=$(( row_idx * 360 / num_rows ))
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
echo "Meteor scan running (tail=$TAIL_LENGTH, delay=$DELAY, rows=$num_rows). Ctrl+C to stop."

frame=0
hue_shift=0
while true; do
    cmd_args=""

    for row_idx in $(seq 0 $((num_rows - 1))); do
        row_len=${ROW_LAMP_COUNT[$row_idx]}
        [[ $row_len -eq 0 ]] && continue

        # Meteor head position for this row (staggered)
        head_pos=$(( (frame + row_idx * STAGGER) % anim_length ))

        # Current hue for this row (shifts over time for rainbow cycling)
        hue=$(( (ROW_HUE[row_idx] + hue_shift) % 360 ))
        hsv2rgb "$hue"
        meteor_r=$_r; meteor_g=$_g; meteor_b=$_b

        # Read lamp IDs for this row into an array
        read -ra row_lamps <<< "${ROW_LAMP_IDS[$row_idx]}"

        for col in $(seq 0 $((row_len - 1))); do
            lamp_id="${row_lamps[$col]}"

            # Distance behind the meteor head
            dist=$((head_pos - col))

            if [[ $dist -eq 0 ]]; then
                # Meteor head: full rainbow color
                hex=$(printf '%02x%02x%02x' "$meteor_r" "$meteor_g" "$meteor_b")
            elif [[ $dist -gt 0 && $dist -le $TAIL_LENGTH ]]; then
                # Tail: fade from rainbow color toward white
                # dist=1 is closest to head (mostly color), dist=TAIL_LENGTH is farthest (mostly white)
                fr=$(lerp_to_white "$meteor_r" "$dist" "$((TAIL_LENGTH + 1))")
                fg=$(lerp_to_white "$meteor_g" "$dist" "$((TAIL_LENGTH + 1))")
                fb=$(lerp_to_white "$meteor_b" "$dist" "$((TAIL_LENGTH + 1))")
                hex=$(printf '%02x%02x%02x' "$fr" "$fg" "$fb")
            else
                # Background: white
                hex="ffffff"
            fi

            cmd_args+="${lamp_id}:${hex} "
        done
    done

    # Send all lamp colors in one command
    # shellcheck disable=SC2086
    hid-rgb-ctl $DEVICE_ARG set-lamp $cmd_args 2>/dev/null

    frame=$((frame + 1))
    # Slowly rotate the rainbow hue assignment so rows cycle colors over time
    if (( frame % 8 == 0 )); then
        hue_shift=$(( (hue_shift + 1) % 360 ))
    fi

    sleep "$DELAY"
done
