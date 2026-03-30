# [hid-rgb-ctl](https://github.com/xz-dev/hid-rgb-ctl)

Linux command-line tool for controlling RGB lighting via standard HID protocols.

> [!NOTE]
> This project was built with **vibe coding**. Contributions via vibe coding
> are welcome — don't worry too much about code style, just make it work.

## Supported Protocols

- **HID LampArray** (Usage Page 0x59) — Modern
  [Dynamic Lighting](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/dynamic-lighting-devices)
  standard from the
  [USB HID Usage Tables v1.4](https://usb.org/document-library/hid-usage-tables-14)
  (Section 26). Supports device interrogation, per-lamp attributes,
  autonomous/manual mode, and color updates.

- **HID LED Page RGB** (Usage Page 0x08, Usage 0x52) — Legacy RGB LED control
  per HID Usage Tables Section 11.7. Simpler protocol with direct R/B/G
  channel writes.

## Features

- Auto-discovers devices by parsing HID report descriptors — no hardcoded
  vendor/product IDs
- Supports preset colors, decimal RGB, hex color codes, and intensity control
- Per-lamp color control via `set-lamp` on LampArray devices
  (LampMultiUpdateReport with automatic batching)
- Toggle autonomous/manual mode on LampArray devices
- Automatic value scaling to device-declared LogicalMaximum
  (e.g. LED Intensity 0-100 per spec)
- Supports both Feature and Output HID report types (auto-detected from
  descriptor)
- Minimal dependencies — only `lexopt` (CLI parsing) and `libc` (ioctl)

## Install

```sh
cargo install hid-rgb-ctl
# or from git
cargo install --git https://github.com/xz-dev/hid-rgb-ctl.git
# or from local clone
cargo install --path .
```

## Usage

```sh
# List detected devices
hid-rgb-ctl list

# Show device attributes and lamp info
hid-rgb-ctl get

# Set color by preset name
hid-rgb-ctl set red

# Set color by RGB values (0-255)
hid-rgb-ctl set 255 165 0

# Set color by hex code
hid-rgb-ctl set ff6400

# Set color with custom intensity
hid-rgb-ctl set cyan -i 128

# Turn off
hid-rgb-ctl set off

# Set per-lamp colors (LampArray only)
hid-rgb-ctl set-lamp 0:red 1:00ff00 2:blue

# Per-lamp with custom intensity
hid-rgb-ctl set-lamp 0:ff0000 1:cyan -i 128

# Specify device path (when multiple devices present)
hid-rgb-ctl -p /dev/hidraw1 set blue

# Toggle autonomous mode (LampArray only)
hid-rgb-ctl auto off    # host takes control
hid-rgb-ctl auto on     # device resumes built-in effects
```

Color presets: `red`, `green`, `blue`, `white`, `cyan`, `yellow`, `orange`,
`purple`, `pink`, `off`

## Permissions

HID device access requires read/write permission on `/dev/hidrawN`.

Add a udev rule for your device, for example:

```sh
# /etc/udev/rules.d/99-hid-rgb.rules
# ASUS Vivobook keyboard (0B05:5570)
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="0b05", ATTRS{idProduct}=="5570", TAG+="uaccess"
```

Then reload:

```sh
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Verified Devices

| Device | Bus | VID:PID | Protocol | Lamps | Status |
|--------|-----|---------|----------|-------|--------|
| ASUS Vivobook S 16 (M5606WA) keyboard | I2C | 0B05:5570 | LampArray | 1 (single-zone) | Verified |

Contributions welcome — please open an issue or PR with your device info.

## Protocol Notes

### LampArray (Usage Page 0x59)

The typical operation flow (Section 26.6):

1. Read `LampArrayAttributesReport` — get lamp count, device kind
2. Read `LampAttributesResponseReport` for each lamp — get position, RGB
   level counts, programmability
3. Disable `AutonomousMode` — take control from device firmware
4. Send `LampRangeUpdateReport` or `LampMultiUpdateReport` with color data
5. Re-enable `AutonomousMode` when done (optional)

`LampCount` (Usage 0x03) tells you how many independently controllable zones
the device has. A single-zone keyboard has `LampCount=1`; a per-key RGB
keyboard may have 100+.

Two update reports are supported:

- **`LampRangeUpdateReport`** (Usage 0x60) — applies a single color to a
  contiguous range of lamps. Used by the `set` command to set all lamps at
  once.
- **`LampMultiUpdateReport`** (Usage 0x50) — updates individual lamps with
  independent colors. Used by the `set-lamp` command. Colors are automatically
  batched based on the device's slot count (derived from report size), with
  intermediate batches setting `LampUpdateComplete=0` and the final batch
  setting `LampUpdateComplete=1` so the device applies all updates atomically.

### LED Page RGB (Usage Page 0x08, Section 11.7)

Simpler protocol — the RGB LED collection (Usage 0x52) directly contains:

- Red LED Channel (Usage 0x53)
- Blue LED Channel (Usage 0x54) — note: Blue before Green in the spec
- Green LED Channel (Usage 0x55)
- LED Intensity (Usage 0x56, optional)

No autonomous mode or lamp enumeration. Both Feature and Output report types
are supported (auto-detected from the descriptor). Color and intensity values
are automatically scaled to the device's declared LogicalMaximum (the spec
recommends logical max 100 for LED Intensity).

### MinUpdateInterval

LampArray devices report a `MinUpdateIntervalInMicroseconds` (visible via
`hid-rgb-ctl get`). When performing rapid sequential updates (e.g. animation
loops), callers should wait at least this long between updates. The spec
requires the host to not send more than one `LampUpdateComplete` per interval.
Since each CLI invocation is a separate process, this cannot be enforced
automatically — scripts should add appropriate delays between calls.

## Examples

See [`examples/`](examples/) for scripts that use `hid-rgb-ctl` for lighting
effects, including `rainbow.sh` (smooth rainbow gradient loop). When writing
animation loops, respect the device's `MinUpdateIntervalInMicroseconds`
(see [MinUpdateInterval](#minupdateinterval) above).

## Library Usage

This crate can also be used as a Rust library:

```rust
use hid_rgb_ctl::{discover_devices, DeviceInfo, LampArrayDevice, LedRgbDevice};

let devices = discover_devices();
for dev in &devices {
    match dev {
        DeviceInfo::LampArray(info) => {
            let device = LampArrayDevice::new(info);
            device.set_color(255, 0, 0, 255).unwrap();
        }
        DeviceInfo::LedRgb(info) => {
            let device = LedRgbDevice::new(info);
            device.set_color(255, 0, 0, 255).unwrap();
        }
    }
}
```

Add to your `Cargo.toml`:

```toml
[dependencies]
hid-rgb-ctl = { git = "https://github.com/xz-dev/hid-rgb-ctl.git" }
```

## References

- [USB HID Usage Tables v1.4](https://www.usb.org/sites/default/files/hut1_4.pdf) —
  Section 26 (Lighting and Illumination), Section 11.7 (Multicolor RGB LED)
- [Microsoft Dynamic Lighting](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/dynamic-lighting-devices) —
  Windows implementation guide for the same HID LampArray standard

## License

MIT
