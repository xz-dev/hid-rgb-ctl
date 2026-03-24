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

_MAIN_DATA_TAGS = frozenset({_TAG_INPUT, _TAG_OUTPUT, _TAG_FEATURE})


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
class _LedRgbChannelBuilder:
    """Accumulates LED RGB channel offsets during descriptor parsing."""

    report_id: int = 0
    report_size: int = 0  # Filled in during finalize()
    red_offset: int | None = None
    blue_offset: int | None = None
    green_offset: int | None = None
    intensity_offset: int | None = None
    channel_size: int = 8

    @property
    def is_complete(self) -> bool:
        """True when all mandatory channels (R, G, B) have been found."""
        return (
            self.red_offset is not None
            and self.blue_offset is not None
            and self.green_offset is not None
        )


# Usage type stored in usages list — either a plain int (usage ID)
# or a tuple ("min", value) awaiting a USAGE_MAX to expand the range.
_UsageEntry = int | tuple[str, int]


@dataclass
class _ParserState:
    """Mutable state for the HID descriptor parser.

    Handles global, local, and main HID items, accumulating results into
    lamp_array_reports and led_rgb_channels.
    """

    # --- Global items (persist across Main items) ---
    usage_page: int = 0
    report_id: int = 0
    report_size: int = 0
    report_count: int = 0
    logical_min: int = 0
    logical_max: int = 0

    # --- Local items (reset after each Main item) ---
    usages: list[_UsageEntry] = field(default_factory=list)

    # --- Accumulated results ---
    lamp_array_reports: dict[str, ReportInfo] = field(default_factory=dict)
    led_rgb_channels: dict[int, _LedRgbChannelBuilder] = field(default_factory=dict)

    # Per-report accumulated data bits for final size calculation
    report_data_bits: dict[int, int] = field(default_factory=dict)

    # Per-(usage_page, report_id) bit offset tracking
    _report_bit_offsets: dict[tuple[int | str, int], int] = field(default_factory=dict)

    # Collection / context tracking
    collection_depth: int = 0
    current_lighting_report_name: str | None = None
    in_rgb_led_collection: bool = False

    # --- Global item handlers ---

    def handle_global(self, tag: int, val: int, payload: bytes) -> None:
        if tag == _TAG_USAGE_PAGE:
            self.usage_page = val
        elif tag == _TAG_REPORT_ID:
            self.report_id = val
        elif tag == _TAG_REPORT_SIZE:
            self.report_size = val
        elif tag == _TAG_REPORT_COUNT:
            self.report_count = val
        elif tag == _TAG_LOGICAL_MIN:
            self.logical_min = _payload_value(payload, signed=True)
        elif tag == _TAG_LOGICAL_MAX:
            self.logical_max = _payload_value(payload, signed=True)

    # --- Local item handlers ---

    def handle_local(self, tag: int, val: int) -> None:
        if tag == _TAG_USAGE:
            self.usages.append(val)
        elif tag == _TAG_USAGE_MIN:
            self.usages.append(("min", val))
        elif tag == _TAG_USAGE_MAX:
            # Expand usage range from the last USAGE_MIN
            if self.usages and isinstance(self.usages[-1], tuple):
                _, umin = self.usages.pop()
                self.usages.extend(range(umin, val + 1))

    # --- Main item handlers ---

    def handle_main(self, tag: int) -> None:
        if tag == _TAG_COLLECTION:
            self._on_collection()
        elif tag == _TAG_END_COLLECTION:
            self._on_end_collection()
        elif tag in _MAIN_DATA_TAGS:
            self._on_data_item()

    def _on_collection(self) -> None:
        self.collection_depth += 1

        if self.usage_page == USAGE_PAGE_LIGHTING:
            for usage in self.usages:
                if isinstance(usage, int) and usage in _LAMP_ARRAY_REPORT_USAGES:
                    self.current_lighting_report_name = _LAMP_ARRAY_REPORT_USAGES[usage]

        if self.usage_page == USAGE_PAGE_LED:
            for usage in self.usages:
                if usage == USAGE_RGB_LED:
                    self.in_rgb_led_collection = True
                    self.led_rgb_channels.setdefault(
                        self.report_id, _LedRgbChannelBuilder()
                    )

        self.usages.clear()

    def _on_end_collection(self) -> None:
        self.collection_depth -= 1
        if self.collection_depth <= 1:
            self.current_lighting_report_name = None
            self.in_rgb_led_collection = False
        self.usages.clear()

    def _on_data_item(self) -> None:
        total_bits = self.report_size * self.report_count
        rid = self.report_id
        self.report_data_bits[rid] = self.report_data_bits.get(rid, 0) + total_bits

        # Lighting Page (0x59): record report info
        if (
            self.usage_page == USAGE_PAGE_LIGHTING
            and self.current_lighting_report_name is not None
        ):
            self.lamp_array_reports.setdefault(
                self.current_lighting_report_name,
                ReportInfo(report_id=rid, size=0),
            )

        # LED Page (0x08): record channel byte offsets
        if self.usage_page == USAGE_PAGE_LED and self.in_rgb_led_collection:
            builder = self.led_rgb_channels.setdefault(rid, _LedRgbChannelBuilder())
            bit_key = ("led", rid)
            current_bits = self._report_bit_offsets.get(bit_key, 0)

            _CHANNEL_SETTERS = {
                USAGE_RED_LED_CHANNEL: "red_offset",
                USAGE_BLUE_LED_CHANNEL: "blue_offset",
                USAGE_GREEN_LED_CHANNEL: "green_offset",
                USAGE_LED_INTENSITY: "intensity_offset",
            }

            for i, usage in enumerate(self.usages):
                if not isinstance(usage, int):
                    continue
                byte_off = (current_bits + i * self.report_size) // 8
                attr = _CHANNEL_SETTERS.get(usage)
                if attr is not None:
                    setattr(builder, attr, byte_off)
                if usage == USAGE_RED_LED_CHANNEL:
                    builder.channel_size = self.report_size

            self._report_bit_offsets[bit_key] = current_bits + total_bits

        self.usages.clear()

    # --- Finalize ---

    def finalize(self) -> tuple[dict[str, ReportInfo], list[_LedRgbChannelBuilder]]:
        """Compute final report sizes and return parsed results."""
        # Fill in byte sizes for LampArray reports
        for rinfo in self.lamp_array_reports.values():
            bits = self.report_data_bits.get(rinfo.report_id, 0)
            rinfo.size = (bits + 7) // 8

        # Collect complete LED RGB channel builders with computed sizes
        complete = []
        for rid, builder in self.led_rgb_channels.items():
            if builder.is_complete:
                builder.report_id = rid
                bits = self.report_data_bits.get(rid, 0)
                builder.report_size = (bits + 7) // 8
                complete.append(builder)

        return self.lamp_array_reports, complete


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
    fmt = {
        1: "<b" if signed else "<B",
        2: "<h" if signed else "<H",
        4: "<i" if signed else "<I",
    }.get(len(payload))
    if fmt is None:
        return int.from_bytes(payload, "little", signed=signed)
    return struct.unpack(fmt, payload)[0]


def _parse_descriptor(
    desc: bytes,
) -> tuple[dict[str, ReportInfo], list[_LedRgbChannelBuilder]]:
    """Parse a binary HID report descriptor.

    Returns:
        lamp_array_reports: dict mapping report name -> ReportInfo
            for Lighting and Illumination Page (0x59)
        led_rgb_builders: list of _LedRgbChannelBuilder with LED Page (0x08)
            RGB LED channel info
    """
    state = _ParserState()
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

        if item_type == 1:  # Global
            state.handle_global(tag, val, payload)
        elif item_type == 2:  # Local
            state.handle_local(tag, val)
        elif item_type == 0:  # Main
            state.handle_main(tag)

    return state.finalize()


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
        lamp_reports, led_rgb_builders = _parse_descriptor(desc_bytes)

        # Check for LampArray (Usage Page 0x59)
        if lamp_reports:
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
        for builder in led_rgb_builders:
            # is_complete guarantees red/blue/green offsets are not None
            assert builder.red_offset is not None
            assert builder.blue_offset is not None
            assert builder.green_offset is not None
            devices.append(
                LedRgbInfo(
                    hidraw_path=f"/dev/{hidraw_name}",
                    name=_get_hid_name(hidraw_name),
                    report_id=builder.report_id,
                    report_size=builder.report_size,
                    red_offset=builder.red_offset,
                    blue_offset=builder.blue_offset,
                    green_offset=builder.green_offset,
                    intensity_offset=builder.intensity_offset,
                    channel_size=builder.channel_size,
                )
            )

    return devices
