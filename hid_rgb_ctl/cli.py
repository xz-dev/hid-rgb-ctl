"""Command-line interface for hid-rgb-ctl."""

from __future__ import annotations

import argparse
import sys

from hid_rgb_ctl import __version__
from hid_rgb_ctl.descriptor import LampArrayInfo, LedRgbInfo, discover_devices
from hid_rgb_ctl.device import LampArrayDevice, LedRgbDevice

PRESETS = {
    "red": (255, 0, 0),
    "green": (0, 255, 0),
    "blue": (0, 0, 255),
    "white": (255, 255, 255),
    "cyan": (0, 255, 255),
    "yellow": (255, 255, 0),
    "orange": (255, 165, 0),
    "purple": (128, 0, 255),
    "pink": (255, 105, 180),
    "off": (0, 0, 0),
}


def _find_device(
    devices: list, path: str | None
) -> LampArrayInfo | LedRgbInfo | None:
    """Find a device by path, or return the first one."""
    if not devices:
        return None
    if path is None:
        return devices[0]
    for d in devices:
        if d.hidraw_path == path:
            return d
    return None


def _make_device(info: LampArrayInfo | LedRgbInfo):
    """Create the appropriate device object from info."""
    if isinstance(info, LampArrayInfo):
        return LampArrayDevice(info)
    return LedRgbDevice(info)


def _parse_color(args: list[str]) -> tuple[int, int, int] | None:
    """Parse color from CLI arguments.

    Accepts:
      - Preset name: "red", "blue", etc.
      - Three decimal values: "255" "100" "0"
      - Six-character hex string: "ff6400"
    """
    if len(args) == 1:
        name = args[0].lower()
        if name in PRESETS:
            return PRESETS[name]
        # Try hex
        s = name.lstrip("#")
        if len(s) == 6:
            try:
                return (int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16))
            except ValueError:
                pass
    elif len(args) == 3:
        try:
            r, g, b = int(args[0]), int(args[1]), int(args[2])
            if all(0 <= v <= 255 for v in (r, g, b)):
                return (r, g, b)
        except ValueError:
            pass
    return None


def cmd_list(devices: list) -> None:
    """List all detected RGB devices."""
    if not devices:
        print("No HID RGB devices found.")
        return

    for d in devices:
        if isinstance(d, LampArrayInfo):
            dev = LampArrayDevice(d)
            try:
                attrs = dev.get_attributes()
                detail = (
                    f"LampArray  {attrs.lamp_count} lamp(s), "
                    f"{attrs.kind_name}"
                )
            except Exception:
                detail = "LampArray"
        else:
            detail = "LED RGB"
        print(f"{d.hidraw_path}  {d.name}  {detail}")


def cmd_get(info: LampArrayInfo | LedRgbInfo) -> None:
    """Show device attributes and lamp info."""
    dev = _make_device(info)

    if isinstance(dev, LampArrayDevice):
        attrs = dev.get_attributes()
        print(f"Device: {dev.name}")
        print(f"Protocol: HID LampArray (Usage Page 0x59)")
        print(f"Path: {dev.path}")
        print(f"Lamps: {attrs.lamp_count}")
        print(f"Kind: {attrs.kind_name}")
        print(
            f"Bounding box: "
            f"{attrs.width_um / 1000:.1f} x "
            f"{attrs.height_um / 1000:.1f} x "
            f"{attrs.depth_um / 1000:.1f} mm"
        )
        print(f"Min update interval: {attrs.min_update_interval_us} us")

        for i in range(attrs.lamp_count):
            lamp = dev.get_lamp(i)
            print(f"\nLamp {lamp.lamp_id}:")
            print(
                f"  Position: ("
                f"{lamp.position_x_um / 1000:.1f}, "
                f"{lamp.position_y_um / 1000:.1f}, "
                f"{lamp.position_z_um / 1000:.1f}) mm"
            )
            print(
                f"  RGB levels: "
                f"{lamp.red_level_count}/"
                f"{lamp.green_level_count}/"
                f"{lamp.blue_level_count}"
            )
            print(f"  Intensity levels: {lamp.intensity_level_count}")
            print(f"  Programmable: {'yes' if lamp.is_programmable else 'no'}")
    else:
        attrs = dev.get_attributes()
        print(f"Device: {attrs.name}")
        print(f"Protocol: {attrs.protocol}")
        print(f"Path: {attrs.path}")
        print(f"Report ID: 0x{attrs.report_id:02x}")
        print(f"Channel size: {attrs.channel_size} bits")
        print(f"Has intensity: {'yes' if attrs.has_intensity else 'no'}")


def cmd_set(
    info: LampArrayInfo | LedRgbInfo, r: int, g: int, b: int, intensity: int = 255
) -> None:
    """Set device color."""
    dev = _make_device(info)
    dev.set_color(r, g, b, intensity)
    msg = f"Set {info.name} to ({r}, {g}, {b})"
    if intensity != 255:
        msg += f" intensity={intensity}"
    print(msg)


def cmd_auto(info: LampArrayInfo | LedRgbInfo, enabled: bool) -> None:
    """Toggle autonomous mode."""
    dev = _make_device(info)
    try:
        dev.set_autonomous(enabled)
        state = "on (device controls)" if enabled else "off (host controls)"
        print(f"Autonomous mode: {state}")
    except NotImplementedError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="hid-rgb-ctl",
        description="Control RGB lighting on HID LampArray and LED Page devices.",
    )
    parser.add_argument(
        "-V", "--version", action="version", version=f"%(prog)s {__version__}"
    )
    parser.add_argument(
        "-p",
        metavar="PATH",
        help="hidraw device path (e.g. /dev/hidraw1). "
        "If omitted, uses the first detected device.",
    )

    sub = parser.add_subparsers(dest="command")

    sub.add_parser("list", help="List detected RGB devices")

    sub.add_parser("get", help="Show device attributes and lamp info")

    set_parser = sub.add_parser("set", help="Set color")
    set_parser.add_argument(
        "color",
        nargs="+",
        help="Color: preset name, R G B (0-255), or hex (e.g. ff6400). "
        f"Presets: {', '.join(PRESETS)}",
    )
    set_parser.add_argument(
        "-i",
        "--intensity",
        type=int,
        default=255,
        metavar="N",
        help="Intensity 0-255 (default: 255). "
        "Maps to IntensityUpdateChannel (Usage 0x54).",
    )

    auto_parser = sub.add_parser("auto", help="Toggle autonomous mode")
    auto_parser.add_argument(
        "state",
        choices=["on", "off"],
        help="on = device controls lamps, off = host controls lamps",
    )

    args = parser.parse_args()

    if args.command is None:
        parser.print_help()
        sys.exit(0)

    devices = discover_devices()

    if args.command == "list":
        cmd_list(devices)
        return

    # All other commands need a device
    info = _find_device(devices, args.p)
    if info is None:
        if args.p:
            print(f"Error: No RGB device found at {args.p}", file=sys.stderr)
        else:
            print("Error: No HID RGB devices found.", file=sys.stderr)
            print(
                "Check permissions on /dev/hidraw* — "
                "see README for udev setup.",
                file=sys.stderr,
            )
        sys.exit(1)

    try:
        if args.command == "get":
            cmd_get(info)
        elif args.command == "set":
            color = _parse_color(args.color)
            if color is None:
                print(
                    f"Error: Invalid color. Use a preset ({', '.join(PRESETS)}), "
                    "R G B values (0-255), or a 6-digit hex code.",
                    file=sys.stderr,
                )
                sys.exit(1)
            cmd_set(info, *color, intensity=args.intensity)
        elif args.command == "auto":
            cmd_auto(info, args.state == "on")
    except PermissionError:
        print(
            f"Error: Permission denied on {info.hidraw_path}.",
            file=sys.stderr,
        )
        print(
            "Run with sudo or set up a udev rule — see README.",
            file=sys.stderr,
        )
        sys.exit(1)
    except OSError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
