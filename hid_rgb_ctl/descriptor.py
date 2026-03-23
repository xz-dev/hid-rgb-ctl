"""HID report descriptor parser and device discovery.

Parses binary HID report descriptors to find devices implementing:
- Usage Page 0x59 (Lighting and Illumination) — HID LampArray protocol
- Usage Page 0x08 (LED Page) with Usage 0x52 (RGB LED) — Legacy RGB LED

Reference: USB HID Usage Tables v1.4
  - Section 26: Lighting and Illumination Page (0x59)
  - Section 11.7: Multicolor (RGB) LED on LED Page (0x08)
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from pathlib import Path

# --- Usage Page constants ---

USAGE_PAGE_LED = 0x08
USAGE_PAGE_LIGHTING = 0x59

# Usage IDs for Lighting and Illumination Page (0x59), Section 26
USAGE_LAMP_ARRAY = 0x01
USAGE_LAMP_ARRAY_ATTRIBUTES_REPORT = 0x02
USAGE_LAMP_COUNT = 0x03
USAGE_LAMP_ATTR_REQUEST_REPORT = 0x20
USAGE_LAMP_ID = 0x21
USAGE_LAMP_ATTR_RESPONSE_REPORT = 0x22
USAGE_LAMP_MULTI_UPDATE_REPORT = 0x50
USAGE_RED_UPDATE_CHANNEL = 0x51
USAGE_GREEN_UPDATE_CHANNEL = 0x52
USAGE_BLUE_UPDATE_CHANNEL = 0x53
USAGE_INTENSITY_UPDATE_CHANNEL = 0x54
USAGE_LAMP_UPDATE_FLAGS = 0x55
USAGE_LAMP_RANGE_UPDATE_REPORT = 0x60
USAGE_LAMP_ID_START = 0x61
USAGE_LAMP_ID_END = 0x62
USAGE_LAMP_ARRAY_CONTROL_REPORT = 0x70
USAGE_AUTONOMOUS_MODE = 0x71

# Usage IDs for LED Page (0x08), Section 11.7
USAGE_RGB_LED = 0x52
USAGE_RED_LED_CHANNEL = 0x53
USAGE_BLUE_LED_CHANNEL = 0x54  # Note: Blue before Green in spec
USAGE_GREEN_LED_CHANNEL = 0x55
USAGE_LED_INTENSITY = 0x56

# Collection types for LampArray report identification
_LAMP_ARRAY_REPORT_USAGES = {
    USAGE_LAMP_ARRAY_ATTRIBUTES_REPORT: "attributes",
    USAGE_LAMP_ATTR_REQUEST_REPORT: "attr_request",
    USAGE_LAMP_ATTR_RESPONSE_REPORT: "attr_response",
    USAGE_LAMP_MULTI_UPDATE_REPORT: "multi_update",
    USAGE_LAMP_RANGE_UPDATE_REPORT: "range_update",
    USAGE_LAMP_ARRAY_CONTROL_REPORT: "control",
}


@dataclass
class ReportInfo:
    """Parsed HID report metadata."""

    report_id: int
    size: int  # Total data bytes (excluding report ID byte)


@dataclass
class LampArrayInfo:
    """Device implementing HID LampArray (Usage Page 0x59).

    Report IDs and sizes are parsed from the device's HID report descriptor,
    not hardcoded, so this works with any compliant LampArray device.
    """

    hidraw_path: str
    name: str
    reports: dict[str, ReportInfo] = field(default_factory=dict)
    # Keys: "attributes", "attr_request", "attr_response",
    #        "multi_update", "range_update", "control"


@dataclass
class LedRgbInfo:
    """Device implementing LED Page RGB LED (Usage Page 0x08, Section 11.7).

    The RGB LED collection (Usage 0x52) contains:
      - Red LED Channel (Usage 0x53)
      - Blue LED Channel (Usage 0x54)  — Note: Blue before Green per spec
      - Green LED Channel (Usage 0x55)
      - LED Intensity (Usage 0x56, optional)

    Byte offsets are parsed from the descriptor to handle any report layout.
    """

    hidraw_path: str
    name: str
    report_id: int
    report_size: int  # Total data bytes (excluding report ID)
    red_offset: int  # Byte offset of Red channel within report data
    blue_offset: int  # Byte offset of Blue channel
    green_offset: int  # Byte offset of Green channel
    intensity_offset: int | None  # Byte offset of Intensity (None if absent)
    channel_size: int  # Bits per channel (typically 8)


# --- HID Report Descriptor Parser ---


@dataclass
class _ParserState:
    """Mutable state for the HID descriptor parser."""

    # Global items (persist across Main items)
    usage_page: int = 0
    report_id: int = 0
    report_size: int = 0
    report_count: int = 0
    logical_min: int = 0
    logical_max: int = 0

    # Local items (reset after each Main item)
    # Stores int (usage ID) or tuple ("min", usage_min) awaiting USAGE_MAX
    usages: list[int | tuple[str, int]] = field(default_factory=list)

    # Tracking for report size calculation
    # Maps (usage_page|"led", report_id) -> accumulated bit offset
    report_bit_offsets: dict[tuple[int | str, int], int] = field(default_factory=dict)

    # Collection depth tracking
    collection_depth: int = 0

    def _report_key(self) -> tuple[int, int]:
        return (self.usage_page, self.report_id)

    def current_bit_offset(self) -> int:
        return self.report_bit_offsets.get(self._report_key(), 0)

    def advance_bits(self, bits: int) -> None:
        key = self._report_key()
        self.report_bit_offsets[key] = self.report_bit_offsets.get(key, 0) + bits

    def clear_local(self) -> None:
        self.usages = []


def _parse_item(data: bytes, offset: int) -> tuple[int, int, bytes]:
    """Parse one HID descriptor item.

    Returns: (tag, item_type, payload_bytes)
    Raises ValueError if data is truncated.
    """
    if offset >= len(data):
        raise ValueError("Unexpected end of descriptor")

    prefix = data[offset]
    size = prefix & 0x03
    if size == 3:
        size = 4  # Size code 3 means 4 bytes
    tag = prefix & 0xFC
    item_type = (prefix >> 2) & 0x03  # 0=Main, 1=Global, 2=Local

    end = offset + 1 + size
    if end > len(data):
        raise ValueError(f"Truncated item at offset {offset}")

    payload = data[offset + 1 : end]
    return tag, item_type, payload


def _payload_value(payload: bytes, signed: bool = False) -> int:
    """Decode a HID item payload as an integer."""
    if not payload:
        return 0
    if signed:
        fmt = {1: "<b", 2: "<h", 4: "<i"}.get(len(payload))
    else:
        fmt = {1: "<B", 2: "<H", 4: "<I"}.get(len(payload))
    if fmt is None:
        return int.from_bytes(payload, "little", signed=signed)
    return struct.unpack(fmt, payload)[0]


# HID item tags (prefix byte with size bits masked out)
# Global items
_TAG_USAGE_PAGE = 0x04
_TAG_LOGICAL_MIN = 0x14
_TAG_LOGICAL_MAX = 0x24
_TAG_REPORT_SIZE = 0x74
_TAG_REPORT_ID = 0x84
_TAG_REPORT_COUNT = 0x94

# Local items
_TAG_USAGE = 0x08
_TAG_USAGE_MIN = 0x18
_TAG_USAGE_MAX = 0x28

# Main items
_TAG_INPUT = 0x80
_TAG_OUTPUT = 0x90
_TAG_FEATURE = 0xB0
_TAG_COLLECTION = 0xA0
_TAG_END_COLLECTION = 0xC0


def _parse_descriptor(
    desc: bytes,
) -> tuple[dict[str, ReportInfo], list[dict]]:
    """Parse a binary HID report descriptor.

    Returns:
        lamp_array_reports: dict mapping report name -> ReportInfo
            for Lighting and Illumination Page (0x59)
        led_rgb_reports: list of dicts with LED Page (0x08) RGB LED info,
            each containing report_id, channel offsets, and sizes
    """
    state = _ParserState()
    lamp_array_reports: dict[str, ReportInfo] = {}
    led_rgb_channels: dict[int, dict] = {}  # report_id -> channel info

    current_lighting_report_name: str | None = None
    in_rgb_led_collection = False

    # Per-report accumulated sizes for final report size calculation
    report_data_bits: dict[int, int] = {}  # report_id -> total data bits

    offset = 0
    while offset < len(desc):
        try:
            tag, item_type, payload = _parse_item(desc, offset)
        except ValueError:
            break

        size = len(payload)
        if (desc[offset] & 0x03) == 3:
            size = 4
        offset += 1 + size
        val = _payload_value(payload)

        # --- Global items ---
        if tag == _TAG_USAGE_PAGE:
            state.usage_page = val
        elif tag == _TAG_REPORT_ID:
            state.report_id = val
        elif tag == _TAG_REPORT_SIZE:
            state.report_size = val
        elif tag == _TAG_REPORT_COUNT:
            state.report_count = val
        elif tag == _TAG_LOGICAL_MIN:
            state.logical_min = _payload_value(payload, signed=True)
        elif tag == _TAG_LOGICAL_MAX:
            state.logical_max = _payload_value(payload, signed=True)

        # --- Local items ---
        elif tag == _TAG_USAGE:
            state.usages.append(val)
        elif tag == _TAG_USAGE_MIN:
            # Wait for USAGE_MAX to expand the range
            state.usages.append(("min", val))
        elif tag == _TAG_USAGE_MAX:
            # Expand usage range from last USAGE_MIN
            if state.usages and isinstance(state.usages[-1], tuple):
                _, umin = state.usages.pop()
                state.usages.extend(range(umin, val + 1))

        # --- Main items ---
        elif tag == _TAG_COLLECTION:
            state.collection_depth += 1
            # Check if this collection starts a known LampArray report
            if state.usage_page == USAGE_PAGE_LIGHTING:
                for usage in state.usages:
                    if isinstance(usage, int) and usage in _LAMP_ARRAY_REPORT_USAGES:
                        current_lighting_report_name = _LAMP_ARRAY_REPORT_USAGES[usage]
            # Check for RGB LED collection on LED Page
            if state.usage_page == USAGE_PAGE_LED:
                for usage in state.usages:
                    if usage == USAGE_RGB_LED:
                        in_rgb_led_collection = True
                        if state.report_id not in led_rgb_channels:
                            led_rgb_channels[state.report_id] = {}
            state.clear_local()

        elif tag == _TAG_END_COLLECTION:
            state.collection_depth -= 1
            if state.collection_depth <= 1:
                current_lighting_report_name = None
            if in_rgb_led_collection and state.collection_depth <= 1:
                in_rgb_led_collection = False
            state.clear_local()

        elif tag in (_TAG_INPUT, _TAG_OUTPUT, _TAG_FEATURE):
            total_bits = state.report_size * state.report_count

            # Track report data sizes
            rid = state.report_id
            report_data_bits[rid] = report_data_bits.get(rid, 0) + total_bits

            # --- Lighting Page (0x59): record report info ---
            if (
                state.usage_page == USAGE_PAGE_LIGHTING
                and current_lighting_report_name is not None
            ):
                rname = current_lighting_report_name
                if rname not in lamp_array_reports:
                    lamp_array_reports[rname] = ReportInfo(
                        report_id=state.report_id, size=0
                    )

            # --- LED Page (0x08): record channel byte offsets ---
            if state.usage_page == USAGE_PAGE_LED and in_rgb_led_collection:
                rid = state.report_id
                if rid not in led_rgb_channels:
                    led_rgb_channels[rid] = {}
                channels = led_rgb_channels[rid]

                # Calculate current byte offset within this report's data
                # We need per-report tracking
                bit_key = ("led", rid)
                current_bits = state.report_bit_offsets.get(bit_key, 0)

                for i, usage in enumerate(state.usages):
                    if not isinstance(usage, int):
                        continue
                    byte_off = (current_bits + i * state.report_size) // 8
                    if usage == USAGE_RED_LED_CHANNEL:
                        channels["red_offset"] = byte_off
                        channels["channel_size"] = state.report_size
                    elif usage == USAGE_BLUE_LED_CHANNEL:
                        channels["blue_offset"] = byte_off
                    elif usage == USAGE_GREEN_LED_CHANNEL:
                        channels["green_offset"] = byte_off
                    elif usage == USAGE_LED_INTENSITY:
                        channels["intensity_offset"] = byte_off

                state.report_bit_offsets[bit_key] = current_bits + total_bits

            state.clear_local()

    # Compute final report sizes (bytes) for LampArray reports
    for rname, rinfo in lamp_array_reports.items():
        bits = report_data_bits.get(rinfo.report_id, 0)
        rinfo.size = (bits + 7) // 8

    # Build LED RGB info list
    led_rgb_reports = []
    for rid, channels in led_rgb_channels.items():
        if "red_offset" in channels and "green_offset" in channels:
            bits = report_data_bits.get(rid, 0)
            led_rgb_reports.append(
                {
                    "report_id": rid,
                    "report_size": (bits + 7) // 8,
                    "channel_size": channels.get("channel_size", 8),
                    **channels,
                }
            )

    return lamp_array_reports, led_rgb_reports


# --- Device Discovery ---


def _get_hid_name(hidraw: str) -> str:
    """Read HID_NAME from the device's uevent file."""
    uevent_path = Path(f"/sys/class/hidraw/{hidraw}/device/uevent")
    try:
        for line in uevent_path.read_text().splitlines():
            if line.startswith("HID_NAME="):
                return line.split("=", 1)[1]
    except OSError:
        pass
    return "Unknown"


def discover_devices() -> list[LampArrayInfo | LedRgbInfo]:
    """Scan all hidraw devices for LampArray and LED RGB support.

    Reads each device's HID report descriptor from sysfs and parses it
    to find devices implementing:
    - Usage Page 0x59 (Lighting and Illumination) — LampArray
    - Usage Page 0x08 (LED Page) with Usage 0x52 (RGB LED)

    Returns a list of LampArrayInfo and/or LedRgbInfo objects.
    """
    devices: list[LampArrayInfo | LedRgbInfo] = []
    hidraw_dir = Path("/sys/class/hidraw")

    if not hidraw_dir.exists():
        return devices

    for entry in sorted(hidraw_dir.iterdir()):
        desc_path = entry / "device" / "report_descriptor"
        if not desc_path.exists():
            continue

        try:
            desc_bytes = desc_path.read_bytes()
        except OSError:
            continue

        if not desc_bytes:
            continue

        hidraw_name = entry.name
        lamp_reports, led_rgb_reports = _parse_descriptor(desc_bytes)

        # Check for LampArray (Usage Page 0x59)
        if lamp_reports:
            # Must have at least the essential reports
            required = {"range_update", "control"}
            if required.issubset(lamp_reports.keys()):
                devices.append(
                    LampArrayInfo(
                        hidraw_path=f"/dev/{hidraw_name}",
                        name=_get_hid_name(hidraw_name),
                        reports=lamp_reports,
                    )
                )

        # Check for LED Page RGB LED (Usage Page 0x08)
        for rgb_info in led_rgb_reports:
            if "blue_offset" not in rgb_info:
                continue
            devices.append(
                LedRgbInfo(
                    hidraw_path=f"/dev/{hidraw_name}",
                    name=_get_hid_name(hidraw_name),
                    report_id=rgb_info["report_id"],
                    report_size=rgb_info["report_size"],
                    red_offset=rgb_info["red_offset"],
                    blue_offset=rgb_info["blue_offset"],
                    green_offset=rgb_info["green_offset"],
                    intensity_offset=rgb_info.get("intensity_offset"),
                    channel_size=rgb_info.get("channel_size", 8),
                )
            )

    return devices
