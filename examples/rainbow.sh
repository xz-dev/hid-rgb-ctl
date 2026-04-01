#!/bin/bash
# Rainbow gradient loop for single-zone RGB keyboards using hid-rgb-ctl.
# Cycles through hues smoothly. Ctrl+C to stop.

STEP=5        # hue increment per frame (1-10, lower = smoother)
DELAY=0.05    # seconds between frames

# HSV to RGB (H: 0-359, S/V fixed at 255)
hsv2rgb() {
    local h=$1
    local region=$((h / 60))
    local remainder=$(( (h % 60) * 255 / 60 ))
    local q=$(( 255 - remainder ))
    local t=$remainder

    case $region in
        0) echo "255 $t 0" ;;
        1) echo "$q 255 0" ;;
        2) echo "0 255 $t" ;;
        3) echo "0 $q 255" ;;
        4) echo "$t 0 255" ;;
        *) echo "255 0 $q" ;;
    esac
}

cleanup() { hid-rgb-ctl auto on 2>/dev/null; echo "Stopped. Auto mode restored."; exit 0; }
trap cleanup INT TERM EXIT

echo "Rainbow loop (step=$STEP, delay=$DELAY). Ctrl+C to stop."

hue=0
while true; do
    read -r r g b <<< "$(hsv2rgb $hue)"
    hid-rgb-ctl set "$r" "$g" "$b" 2>/dev/null
    hue=$(( (hue + STEP) % 360 ))
    sleep "$DELAY"
done
