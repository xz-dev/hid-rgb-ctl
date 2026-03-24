"""HID RGB device control.

Provides two device classes:
- LampArrayDevice: HID LampArray (Usage Page 0x59) per HUT v1.4 Section 26
- LedRgbDevice: LED Page RGB LED (Usage Page 0x08) per HUT v1.4 Section 11.7
"""

from __future__ import annotations

import fcntl
import io
import struct
import warnings
from dataclasses import dataclass

from hid_rgb_ctl.descriptor import LampArrayInfo, LedRgbInfo

# Linux HIDRAW ioctl numbers
# HIDIOCGFEATURE = _IOC(_IOC_READ|_IOC_WRITE, 'H', 0x07, len)
# HIDIOCSFEATURE = _IOC(_IOC_READ|_IOC_WRITE, 'H', 0x06, len)


def HIDIOCGFEATURE(size: int) -> int:
    return 0xC0004807 | (size << 16)


def HIDIOCSFEATURE(size: int) -> int:
    return 0xC0004806 | (size << 16)


# LampArrayKind values (Section 26.2.1)
LAMP_ARRAY_KINDS = {
    0: "Undefined",
    1: "Keyboard",
    2: "Mouse",
    3: "GameController",
    4: "Peripheral",
    5: "Scene",
    6: "Notification",
    7: "Chassis",
    8: "Wearable",
    9: "Furniture",
    10: "Art",
}


@dataclass
class LampAttributes:
    """Attributes of a single lamp (Section 26.3)."""

    lamp_id: int
    position_x_um: int  # micrometers
    position_y_um: int
    position_z_um: int
    update_latency_us: int  # microseconds
    lamp_purposes: int
    red_level_count: int
    green_level_count: int
    blue_level_count: int
    intensity_level_count: int
    is_programmable: bool
    input_binding: int


@dataclass
class LampArrayAttributes:
    """LampArray device attributes (Section 26.2)."""

    lamp_count: int
    width_um: int  # bounding box in micrometers
    height_um: int
    depth_um: int
    kind: int
    kind_name: str
    min_update_interval_us: int


@dataclass
class LedRgbAttributes:
    """LED Page RGB device attributes (Section 11.7)."""

    name: str
    path: str
    protocol: str
    report_id: int
    channel_size: int
    has_intensity: bool


class _HidDevice:
    """Base for hidraw devices with shared ioctl helpers and context manager.

    Use as a context manager to batch multiple ioctl calls on a single fd::

        with device:
            device.set_autonomous(False)
            device.set_color(255, 0, 0)

    Without the context manager, each call opens/closes the fd individually.
    """

    def __init__(self, info: LampArrayInfo | LedRgbInfo) -> None:
        self.info = info
        self.path = info.hidraw_path
        self.name = info.name
        self._fd: io.FileIO | None = None

    # -- context manager for fd batching --

    def __enter__(self) -> _HidDevice:
        self._fd = open(self.path, "r+b", buffering=0)
        return self

    def __exit__(self, *exc: object) -> None:
        if self._fd is not None:
            self._fd.close()
            self._fd = None

    # -- low-level HID feature report I/O --

    def _ioctl(self, request: int, buf: bytearray) -> None:
        """Issue an ioctl, using the batched fd if open, else a one-shot fd."""
        if self._fd is not None:
            fcntl.ioctl(self._fd, request, buf)
        else:
            with open(self.path, "r+b", buffering=0) as f:
                fcntl.ioctl(f, request, buf)

    def _feat_get(self, report_id: int, size: int) -> bytearray:
        """Read a HID Feature report."""
        buf = bytearray(size + 1)  # +1 for report ID
        buf[0] = report_id
        self._ioctl(HIDIOCGFEATURE(len(buf)), buf)
        return buf

    def _feat_set(self, buf: bytearray) -> None:
        """Write a HID Feature report."""
        self._ioctl(HIDIOCSFEATURE(len(buf)), buf)


class LampArrayDevice(_HidDevice):
    """HID LampArray (Usage Page 0x59) control.

    Implements the LampArray operation flow per HUT v1.4 Section 26.6:
    interrogation -> disable autonomous -> update lamps -> (re-enable autonomous)

    Report IDs and sizes come from descriptor parsing, not hardcoded.
    """

    def __init__(self, info: LampArrayInfo) -> None:
        super().__init__(info)
        self._reports = info.reports

    def _require_report(self, name: str):
        """Return a report's info or raise with a helpful message."""
        rinfo = self._reports.get(name)
        if rinfo is None:
            raise RuntimeError(f"Device has no {name!r} report")
        return rinfo

    def get_attributes(self) -> LampArrayAttributes:
        """Read LampArrayAttributesReport (Section 26.2).

        Returns lamp count, bounding box dimensions, device kind,
        and minimum update interval.
        """
        rinfo = self._require_report("attributes")
        buf = self._feat_get(rinfo.report_id, rinfo.size)

        # Layout: [ReportID, LampCount(16), Width(32), Height(32),
        #          Depth(32), Kind(32), MinInterval(32)]
        lamp_count = struct.unpack_from("<H", buf, 1)[0]
        width, height, depth, kind, interval = struct.unpack_from("<IIIII", buf, 3)

        return LampArrayAttributes(
            lamp_count=lamp_count,
            width_um=width,
            height_um=height,
            depth_um=depth,
            kind=kind,
            kind_name=LAMP_ARRAY_KINDS.get(kind, f"Unknown({kind})"),
            min_update_interval_us=interval,
        )

    def get_lamp(self, index: int) -> LampAttributes:
        """Read attributes for a single lamp (Section 26.3).

        Sends LampAttributesRequestReport with the lamp index,
        then reads LampAttributesResponseReport.
        """
        req_info = self._require_report("attr_request")
        req_buf = bytearray(req_info.size + 1)
        req_buf[0] = req_info.report_id
        struct.pack_into("<H", req_buf, 1, index)
        self._feat_set(req_buf)

        resp_info = self._require_report("attr_response")
        buf = self._feat_get(resp_info.report_id, resp_info.size)

        # Layout: [ReportID, LampId(16), PosX(32), PosY(32), PosZ(32),
        #          Latency(32), Purposes(32), RedCount(8), GreenCount(8),
        #          BlueCount(8), IntensityCount(8), IsProgrammable(8),
        #          InputBinding(8)]
        lamp_id = struct.unpack_from("<H", buf, 1)[0]
        pos_x, pos_y, pos_z, latency, purposes = struct.unpack_from("<IIIII", buf, 3)
        r_cnt, g_cnt, b_cnt, i_cnt, prog, binding = struct.unpack_from(
            "<BBBBBB", buf, 23
        )

        return LampAttributes(
            lamp_id=lamp_id,
            position_x_um=pos_x,
            position_y_um=pos_y,
            position_z_um=pos_z,
            update_latency_us=latency,
            lamp_purposes=purposes,
            red_level_count=r_cnt,
            green_level_count=g_cnt,
            blue_level_count=b_cnt,
            intensity_level_count=i_cnt,
            is_programmable=bool(prog),
            input_binding=binding,
        )

    def set_autonomous(self, enabled: bool) -> None:
        """Toggle AutonomousMode (Section 26.5, 26.10.1).

        When True: device controls lamps autonomously (built-in effects).
        When False: host has exclusive control, device ignores its own effects.
        Default device state is True (autonomous).
        """
        ctrl_info = self._require_report("control")
        buf = bytearray(ctrl_info.size + 1)
        buf[0] = ctrl_info.report_id
        buf[1] = 0x01 if enabled else 0x00
        self._feat_set(buf)

    def set_color(self, r: int, g: int, b: int, intensity: int = 255) -> None:
        """Set all lamps to a uniform color.

        Disables autonomous mode, then sends a LampRangeUpdate covering
        all lamps (LampIdStart=0, LampIdEnd=LampCount-1) with
        LampUpdateComplete=1 to apply immediately.

        Opens the fd once for the entire sequence (autonomous + attrs + update).
        """
        range_info = self._require_report("range_update")

        with self:
            self.set_autonomous(False)

            try:
                attrs = self.get_attributes()
                lamp_end = max(0, attrs.lamp_count - 1)
            except OSError as exc:
                warnings.warn(
                    f"Could not read lamp count, defaulting to 0: {exc}",
                    RuntimeWarning,
                    stacklevel=2,
                )
                lamp_end = 0

            # LampRangeUpdateReport layout (Section 26.4):
            # [ReportID, Flags(8), IdStart(16), IdEnd(16), R(8), G(8), B(8), I(8)]
            buf = bytearray(range_info.size + 1)
            struct.pack_into(
                "<BBHHBBBB",
                buf,
                0,
                range_info.report_id,
                0x01,  # LampUpdateFlags: bit 0 = LampUpdateComplete
                0,  # LampIdStart
                lamp_end,  # LampIdEnd
                r & 0xFF,
                g & 0xFF,
                b & 0xFF,
                intensity & 0xFF,
            )
            self._feat_set(buf)

    def summary(self) -> str:
        """One-line summary for CLI listing."""
        try:
            attrs = self.get_attributes()
            return f"LampArray  {attrs.lamp_count} lamp(s), {attrs.kind_name}"
        except OSError:
            return "LampArray"


class LedRgbDevice(_HidDevice):
    """HID LED Page RGB LED (Usage Page 0x08, Section 11.7) control.

    Uses the RGB LED collection (Usage 0x52) with individual channel controls:
      - Red LED Channel (Usage 0x53)
      - Blue LED Channel (Usage 0x54)  — Note: spec order is R, B, G
      - Green LED Channel (Usage 0x55)
      - LED Intensity (Usage 0x56, optional)

    Byte offsets are determined by descriptor parsing, not assumed.
    """

    def set_color(self, r: int, g: int, b: int, intensity: int = 255) -> None:
        """Set RGB LED color via Feature report.

        Maps arguments to the correct channel offsets parsed from the descriptor.
        Note the spec channel order is R(0x53), B(0x54), G(0x55) — we handle
        the mapping internally so callers always pass (r, g, b).
        """
        info = self.info
        buf = bytearray(info.report_size + 1)
        buf[0] = info.report_id
        buf[1 + info.red_offset] = r & 0xFF
        buf[1 + info.blue_offset] = b & 0xFF
        buf[1 + info.green_offset] = g & 0xFF
        if info.intensity_offset is not None:
            buf[1 + info.intensity_offset] = intensity & 0xFF
        self._feat_set(buf)

    def get_attributes(self) -> LedRgbAttributes:
        """Return basic device info (LED Page has no LampArray-style attributes)."""
        return LedRgbAttributes(
            name=self.name,
            path=self.path,
            protocol="LED Page RGB (Usage Page 0x08, Section 11.7)",
            report_id=self.info.report_id,
            channel_size=self.info.channel_size,
            has_intensity=self.info.intensity_offset is not None,
        )

    def summary(self) -> str:
        """One-line summary for CLI listing."""
        return "LED RGB"
