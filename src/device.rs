//! HID RGB device control.
//!
//! Provides two device types:
//! - [`LampArrayDevice`]: HID LampArray (Usage Page 0x59) per HUT v1.4 Section 26
//! - [`LedRgbDevice`]: LED Page RGB LED (Usage Page 0x08) per HUT v1.4 Section 11.7

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;

use crate::descriptor::{LampArrayInfo, LedRgbInfo, ReportInfo, ReportType};
use crate::error::{Error, Result};

// Linux HIDRAW ioctl numbers
// HIDIOCGFEATURE = _IOC(_IOC_READ|_IOC_WRITE, 'H', 0x07, len)
// HIDIOCSFEATURE = _IOC(_IOC_READ|_IOC_WRITE, 'H', 0x06, len)

fn hidiocgfeature(size: usize) -> libc::c_ulong {
    0xC000_4807 | ((size as libc::c_ulong) << 16)
}

fn hidiocsfeature(size: usize) -> libc::c_ulong {
    0xC000_4806 | ((size as libc::c_ulong) << 16)
}

/// Scale a u8 value from 0-255 range to 0-logical_max range.
///
/// When `logical_max` is 255, returns the input unchanged.
/// When `logical_max` is 100 (e.g. LED Intensity per Section 11.7),
/// scales proportionally: 255 -> 100, 128 -> 50, etc.
fn scale_u8(value: u8, logical_max: u32) -> u8 {
    if logical_max == 0 {
        return 0;
    }
    if logical_max >= 255 {
        return value;
    }
    ((value as u32 * logical_max + 127) / 255) as u8
}

/// LampArrayKind values (Section 26.2.1).
pub fn lamp_array_kind_name(kind: u32) -> &'static str {
    match kind {
        0 => "Undefined",
        1 => "Keyboard",
        2 => "Mouse",
        3 => "GameController",
        4 => "Peripheral",
        5 => "Scene",
        6 => "Notification",
        7 => "Chassis",
        8 => "Wearable",
        9 => "Furniture",
        10 => "Art",
        11 => "Headset",
        0x100.. => "Vendor-defined",
        _ => "Reserved",
    }
}

/// Attributes of a single lamp (Section 26.3).
#[derive(Debug, Clone)]
pub struct LampAttributes {
    pub lamp_id: u16,
    /// Position in micrometers.
    pub position_x_um: u32,
    pub position_y_um: u32,
    pub position_z_um: u32,
    /// Update latency in microseconds.
    pub update_latency_us: u32,
    pub lamp_purposes: u32,
    pub red_level_count: u8,
    pub green_level_count: u8,
    pub blue_level_count: u8,
    pub intensity_level_count: u8,
    pub is_programmable: bool,
    pub input_binding: u8,
}

/// Per-lamp color specification for [`LampArrayDevice::set_lamp_colors`].
#[derive(Debug, Clone, Copy)]
pub struct LampColor {
    pub lamp_id: u16,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub intensity: u8,
}

/// LampArray device attributes (Section 26.2).
#[derive(Debug, Clone)]
pub struct LampArrayAttributes {
    pub lamp_count: u16,
    /// Bounding box in micrometers.
    pub width_um: u32,
    pub height_um: u32,
    pub depth_um: u32,
    pub kind: u32,
    pub kind_name: &'static str,
    pub min_update_interval_us: u32,
}

/// LED Page RGB device attributes (Section 11.7).
#[derive(Debug, Clone)]
pub struct LedRgbAttributes {
    pub name: String,
    pub path: String,
    pub protocol: &'static str,
    pub report_id: u8,
    pub channel_size: u32,
    pub has_intensity: bool,
}

// --- Low-level ioctl helpers ---

/// File descriptor wrapper for batched ioctl operations.
struct HidrawFd {
    fd: std::fs::File,
}

impl HidrawFd {
    fn open(path: &str) -> Result<Self> {
        let fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => Error::PermissionDenied {
                    path: path.to_string(),
                },
                std::io::ErrorKind::NotFound => Error::DeviceNotFound {
                    path: path.to_string(),
                },
                _ => Error::Io(e),
            })?;
        Ok(Self { fd })
    }

    /// Read a HID Feature report (HIDIOCGFEATURE).
    fn feat_get(&self, report_id: u8, size: usize) -> Result<Vec<u8>> {
        let buf_len = size + 1; // +1 for report ID
        let mut buf = vec![0u8; buf_len];
        buf[0] = report_id;
        let ret = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                hidiocgfeature(buf_len),
                buf.as_mut_ptr(),
            )
        };
        if ret < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // Truncate to the actual number of bytes returned by the kernel.
        // The Linux HIDRAW driver returns the real transfer size; if the
        // device responded with fewer bytes the remainder is undefined.
        // Callers (parse_lamp_response, get_attributes_with_fd) validate
        // the resulting length and produce TruncatedReport errors as needed.
        buf.truncate(ret as usize);
        Ok(buf)
    }

    /// Write a HID Feature report (HIDIOCSFEATURE).
    fn feat_set(&self, buf: &[u8]) -> Result<()> {
        let ret =
            unsafe { libc::ioctl(self.fd.as_raw_fd(), hidiocsfeature(buf.len()), buf.as_ptr()) };
        if ret < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    /// Write a HID Output report via write() syscall.
    ///
    /// On Linux HIDRAW, Output reports are sent by writing directly to the fd
    /// (as opposed to Feature reports which use ioctl).
    fn output_set(&self, buf: &[u8]) -> Result<()> {
        use std::io::Write;
        (&self.fd).write_all(buf)?;
        Ok(())
    }
}

// --- Helper to require a report ---

fn require_report<'a>(report: &'a Option<ReportInfo>, name: &str) -> Result<&'a ReportInfo> {
    report.as_ref().ok_or_else(|| Error::MissingReport {
        report_name: name.to_string(),
    })
}

// --- Lamp response parser ---

/// Parse a LampAttributesResponseReport buffer into [`LampAttributes`].
///
/// Layout (Section 26.3, verified against Microsoft reference
/// `LampArrayReportDescriptor.h` Report 3):
///   `[ReportID, LampId(16), PosX(32), PosY(32), PosZ(32),
///    Latency(32), Purposes(32), RedCount(8), GreenCount(8),
///    BlueCount(8), IntensityCount(8), IsProgrammable(8), InputBinding(8)]`
fn parse_lamp_response(buf: &[u8]) -> Result<LampAttributes> {
    // Minimum size: ReportId(1) + LampId(2) + Pos(12) + Latency(4) + Purposes(4)
    //               + RGBI counts(4) + IsProgrammable(1) + InputBinding(1) = 29 bytes
    // Verified against Microsoft reference LampAttributesResponseReport struct.
    if buf.len() < 29 {
        return Err(Error::TruncatedReport {
            report_name: "LampAttributesResponse",
            expected: 29,
            got: buf.len(),
        });
    }
    let lamp_id = u16::from_le_bytes([buf[1], buf[2]]);
    let pos_x = u32::from_le_bytes([buf[3], buf[4], buf[5], buf[6]]);
    let pos_y = u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]);
    let pos_z = u32::from_le_bytes([buf[11], buf[12], buf[13], buf[14]]);
    let latency = u32::from_le_bytes([buf[15], buf[16], buf[17], buf[18]]);
    let purposes = u32::from_le_bytes([buf[19], buf[20], buf[21], buf[22]]);

    Ok(LampAttributes {
        lamp_id,
        position_x_um: pos_x,
        position_y_um: pos_y,
        position_z_um: pos_z,
        update_latency_us: latency,
        lamp_purposes: purposes,
        red_level_count: buf[23],
        green_level_count: buf[24],
        blue_level_count: buf[25],
        intensity_level_count: buf[26],
        is_programmable: buf[27] != 0,
        input_binding: buf[28],
    })
}

// --- LampArrayDevice ---

/// HID LampArray (Usage Page 0x59) control.
///
/// Implements the LampArray operation flow per HUT v1.4 Section 26.6:
/// interrogation -> disable autonomous -> update lamps -> (re-enable autonomous)
///
/// Report IDs and sizes come from descriptor parsing, not hardcoded.
pub struct LampArrayDevice<'a> {
    info: &'a LampArrayInfo,
}

impl<'a> LampArrayDevice<'a> {
    pub fn new(info: &'a LampArrayInfo) -> Self {
        Self { info }
    }

    /// The device path (e.g. `/dev/hidraw0`).
    pub fn path(&self) -> &str {
        &self.info.hidraw_path
    }

    /// The device name from sysfs.
    pub fn name(&self) -> &str {
        &self.info.name
    }

    /// Read LampArrayAttributesReport (Section 26.2).
    ///
    /// Returns lamp count, bounding box dimensions, device kind,
    /// and minimum update interval.
    pub fn get_attributes(&self) -> Result<LampArrayAttributes> {
        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        self.get_attributes_with_fd(&fd)
    }

    fn get_attributes_with_fd(&self, fd: &HidrawFd) -> Result<LampArrayAttributes> {
        let rinfo = require_report(&self.info.reports.attributes, "attributes")?;
        let buf = fd.feat_get(rinfo.report_id, rinfo.size)?;

        // Minimum size: ReportId(1) + LampCount(2) + 5×u32(20) = 23 bytes
        // Verified against Microsoft reference LampArrayAttributesReport struct.
        if buf.len() < 23 {
            return Err(Error::TruncatedReport {
                report_name: "LampArrayAttributes",
                expected: 23,
                got: buf.len(),
            });
        }

        // Layout: [ReportID, LampCount(16), Width(32), Height(32),
        //          Depth(32), Kind(32), MinInterval(32)]
        let lamp_count = u16::from_le_bytes([buf[1], buf[2]]);
        let width = u32::from_le_bytes([buf[3], buf[4], buf[5], buf[6]]);
        let height = u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]);
        let depth = u32::from_le_bytes([buf[11], buf[12], buf[13], buf[14]]);
        let kind = u32::from_le_bytes([buf[15], buf[16], buf[17], buf[18]]);
        let interval = u32::from_le_bytes([buf[19], buf[20], buf[21], buf[22]]);

        Ok(LampArrayAttributes {
            lamp_count,
            width_um: width,
            height_um: height,
            depth_um: depth,
            kind,
            kind_name: lamp_array_kind_name(kind),
            min_update_interval_us: interval,
        })
    }

    /// Read attributes for a single lamp (Section 26.3).
    ///
    /// Sends LampAttributesRequestReport with the lamp index,
    /// then reads LampAttributesResponseReport.
    pub fn get_lamp(&self, index: u16) -> Result<LampAttributes> {
        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        self.get_lamp_with_fd(&fd, index)
    }

    fn get_lamp_with_fd(&self, fd: &HidrawFd, index: u16) -> Result<LampAttributes> {
        let req_info = require_report(&self.info.reports.attr_request, "attr_request")?;
        let mut req_buf = vec![0u8; req_info.size + 1];
        req_buf[0] = req_info.report_id;
        let idx_bytes = index.to_le_bytes();
        req_buf[1] = idx_bytes[0];
        req_buf[2] = idx_bytes[1];
        fd.feat_set(&req_buf)?;

        let resp_info = require_report(&self.info.reports.attr_response, "attr_response")?;
        let buf = fd.feat_get(resp_info.report_id, resp_info.size)?;
        let lamp = parse_lamp_response(&buf)?;

        // Per Section 26.8.2: "The Host must always check the LampId of
        // the returned report to ensure it was expected."
        if lamp.lamp_id != index {
            return Err(Error::LampIdMismatch {
                expected: index,
                got: lamp.lamp_id,
            });
        }

        Ok(lamp)
    }

    /// Read all lamp attributes using the auto-increment mechanism (Section 26.8.2).
    ///
    /// Sends a single `LampAttributesRequestReport` for LampId=0, then reads
    /// `lamp_count` consecutive `LampAttributesResponseReport`s. The device
    /// auto-increments its internal LampId after each successful response,
    /// reducing the number of ioctl calls from 2N to N+1.
    ///
    /// Each response's LampId is validated against the expected sequence.
    fn read_all_lamps_with_fd(
        &self,
        fd: &HidrawFd,
        lamp_count: u16,
    ) -> Result<Vec<LampAttributes>> {
        if lamp_count == 0 {
            return Ok(Vec::new());
        }

        // Send request for lamp 0 (sets device internal counter).
        let req_info = require_report(&self.info.reports.attr_request, "attr_request")?;
        let mut req_buf = vec![0u8; req_info.size + 1];
        req_buf[0] = req_info.report_id;
        // LampId = 0 (already zeroed)
        fd.feat_set(&req_buf)?;

        // Read lamp_count responses; device auto-increments after each.
        let resp_info = require_report(&self.info.reports.attr_response, "attr_response")?;
        let mut lamps = Vec::with_capacity(lamp_count as usize);

        for expected_id in 0..lamp_count {
            let buf = fd.feat_get(resp_info.report_id, resp_info.size)?;
            let lamp = parse_lamp_response(&buf)?;
            if lamp.lamp_id != expected_id {
                return Err(Error::LampIdMismatch {
                    expected: expected_id,
                    got: lamp.lamp_id,
                });
            }
            lamps.push(lamp);
        }

        Ok(lamps)
    }

    /// Read attributes and all lamp info using a single fd.
    ///
    /// Uses the auto-increment mechanism (Section 26.8.2) to read all
    /// lamp attributes efficiently with a single request followed by
    /// sequential responses.
    pub fn get_attributes_and_lamps(&self) -> Result<(LampArrayAttributes, Vec<LampAttributes>)> {
        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        let attrs = self.get_attributes_with_fd(&fd)?;
        let lamps = self.read_all_lamps_with_fd(&fd, attrs.lamp_count)?;
        Ok((attrs, lamps))
    }

    /// Toggle AutonomousMode (Section 26.5, 26.10.1).
    ///
    /// When `true`: device controls lamps autonomously (built-in effects).
    /// When `false`: host has exclusive control, device ignores its own effects.
    /// Default device state is `true` (autonomous).
    pub fn set_autonomous(&self, enabled: bool) -> Result<()> {
        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        self.set_autonomous_with_fd(&fd, enabled)
    }

    fn set_autonomous_with_fd(&self, fd: &HidrawFd, enabled: bool) -> Result<()> {
        let ctrl_info = require_report(&self.info.reports.control, "control")?;
        let mut buf = vec![0u8; ctrl_info.size + 1];
        buf[0] = ctrl_info.report_id;
        buf[1] = if enabled { 0x01 } else { 0x00 };
        fd.feat_set(&buf)
    }

    /// Try to set AutonomousMode; silently succeeds if the device has
    /// no LampArrayControlReport (Section 26.10.1: "If this field is
    /// absent, it means no autonomous mode is supported.").
    fn try_set_autonomous_with_fd(&self, fd: &HidrawFd, enabled: bool) -> Result<()> {
        let ctrl_info = match &self.info.reports.control {
            Some(info) => info,
            None => return Ok(()),
        };
        let mut buf = vec![0u8; ctrl_info.size + 1];
        buf[0] = ctrl_info.report_id;
        buf[1] = if enabled { 0x01 } else { 0x00 };
        fd.feat_set(&buf)
    }

    /// Set all lamps to a uniform color.
    ///
    /// Disables autonomous mode, reads all lamp attributes, scales the
    /// RGBI values to the device's LevelCounts (Section 26.9), then sends
    /// a LampRangeUpdate covering all lamps with LampUpdateComplete=1.
    ///
    /// RGB values are scaled to the minimum LevelCount across all
    /// *Programmable* lamps in the range (FixedColor lamps' RGB channels
    /// are ignored by the device per Section 26.11.2). Intensity is scaled
    /// to the minimum IntensityLevelCount across *all* lamps.
    ///
    /// Opens the fd once for the entire sequence.
    ///
    /// Note: Callers performing rapid sequential updates should respect
    /// the device's `min_update_interval_us` (from [`get_attributes()`])
    /// between calls. Per Section 26.11, the spec requires no more than
    /// one LampUpdateComplete per MinUpdateIntervalInMicroseconds.
    pub fn set_color(&self, r: u8, g: u8, b: u8, intensity: u8) -> Result<()> {
        let range_info = require_report(&self.info.reports.range_update, "range_update")?;
        let range_report_id = range_info.report_id;
        let range_size = range_info.size;

        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        self.try_set_autonomous_with_fd(&fd, false)?;

        let attrs = self.get_attributes_with_fd(&fd)?;
        if attrs.lamp_count == 0 {
            return Ok(());
        }
        let lamp_end = attrs.lamp_count - 1;

        // Read all lamp attributes for LevelCount scaling.
        let lamps = self.read_all_lamps_with_fd(&fd, attrs.lamp_count)?;

        // Compute scaling limits.
        // RGB: min LevelCount across Programmable lamps only (Section 26.11.2:
        //   "For FixedColor Lamps, Red/Green/Blue channels are always ignored.")
        // Intensity: min across all lamps (FixedColor lamps support intensity).
        let (max_r, max_g, max_b) = lamps.iter().filter(|l| l.is_programmable).fold(
            (255u32, 255u32, 255u32),
            |(mr, mg, mb), l| {
                (
                    mr.min(l.red_level_count as u32),
                    mg.min(l.green_level_count as u32),
                    mb.min(l.blue_level_count as u32),
                )
            },
        );
        let max_i = lamps
            .iter()
            .map(|l| l.intensity_level_count as u32)
            .min()
            .unwrap_or(255);

        // LampRangeUpdateReport layout (Section 26.11.2, verified against
        // Microsoft reference Report 5):
        // [ReportID, Flags(8), IdStart(16), IdEnd(16), R(8), G(8), B(8), I(8)]
        let mut buf = vec![0u8; range_size + 1];
        buf[0] = range_report_id;
        buf[1] = 0x01; // LampUpdateFlags: bit 0 = LampUpdateComplete
        let start_bytes = 0u16.to_le_bytes();
        buf[2] = start_bytes[0];
        buf[3] = start_bytes[1];
        let end_bytes = lamp_end.to_le_bytes();
        buf[4] = end_bytes[0];
        buf[5] = end_bytes[1];
        buf[6] = scale_u8(r, max_r);
        buf[7] = scale_u8(g, max_g);
        buf[8] = scale_u8(b, max_b);
        buf[9] = scale_u8(intensity, max_i);

        fd.feat_set(&buf)
    }

    /// Set individual lamp colors using LampMultiUpdateReport (Section 26.11.1).
    ///
    /// Each entry is `(lamp_id, red, green, blue, intensity)`.
    ///
    /// Before sending, this method:
    /// - Validates all LampIds are within the device's LampCount range
    ///   (Section 26.11.1: "Any LampId >= Device LampCount" is an error)
    /// - Rejects duplicate LampIds within a single call
    ///   (Section 26.11.1: "Identical LampId in multiple slots" is an error)
    /// - Scales RGBI values to each lamp's declared LevelCounts (Section 26.9)
    /// - Zeros RGB channels for FixedColor lamps (Section 26.11.1 best practice)
    ///
    /// Colors are batched into reports based on the device's slot count.
    /// Intermediate batches set LampUpdateComplete=0; the final batch sets
    /// LampUpdateComplete=1 so the device applies all updates atomically.
    ///
    /// Requires the device descriptor to include a `multi_update` report.
    ///
    /// Note: Callers performing rapid sequential updates should respect
    /// the device's `min_update_interval_us` (from [`get_attributes()`])
    /// between calls. Per Section 26.11, the spec requires no more than
    /// one LampUpdateComplete per MinUpdateIntervalInMicroseconds.
    pub fn set_lamp_colors(&self, colors: &[LampColor]) -> Result<()> {
        if colors.is_empty() {
            return Ok(());
        }

        let multi_info = require_report(&self.info.reports.multi_update, "multi_update")?;
        let multi_report_id = multi_info.report_id;
        let multi_size = multi_info.size;

        // Derive slot count from report data size:
        //   data = LampCount(1) + Flags(1) + N×LampId(2) + N×RGBI(4) = 2 + 6N
        let slot_count = (multi_size.saturating_sub(2)) / 6;
        if slot_count == 0 {
            return Err(Error::MissingReport {
                report_name: "multi_update (invalid size)".to_string(),
            });
        }

        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        self.try_set_autonomous_with_fd(&fd, false)?;

        // Read device attributes and all lamp attributes for validation + scaling.
        let attrs = self.get_attributes_with_fd(&fd)?;
        let lamps = self.read_all_lamps_with_fd(&fd, attrs.lamp_count)?;

        // Validate: all LampIds must be < LampCount (Section 26.11.1).
        for c in colors {
            if c.lamp_id >= attrs.lamp_count {
                return Err(Error::LampIdOutOfRange {
                    lamp_id: c.lamp_id,
                    lamp_count: attrs.lamp_count,
                });
            }
        }

        // Validate: no duplicate LampIds (Section 26.11.1).
        let mut seen = HashSet::with_capacity(colors.len());
        for c in colors {
            if !seen.insert(c.lamp_id) {
                return Err(Error::DuplicateLampId { lamp_id: c.lamp_id });
            }
        }

        // Pre-compute scaled colors per lamp.
        // Programmable lamps: scale RGBI to individual LevelCounts.
        // FixedColor lamps: set RGB to 0, scale Intensity only (Section 26.11.1).
        let scaled: Vec<LampColor> = colors
            .iter()
            .map(|c| {
                let lamp = &lamps[c.lamp_id as usize];
                if lamp.is_programmable {
                    LampColor {
                        lamp_id: c.lamp_id,
                        red: scale_u8(c.red, lamp.red_level_count as u32),
                        green: scale_u8(c.green, lamp.green_level_count as u32),
                        blue: scale_u8(c.blue, lamp.blue_level_count as u32),
                        intensity: scale_u8(c.intensity, lamp.intensity_level_count as u32),
                    }
                } else {
                    // FixedColor: "as a best practice these channels should
                    // always be set to 0 by the Host" (Section 26.11.1)
                    LampColor {
                        lamp_id: c.lamp_id,
                        red: 0,
                        green: 0,
                        blue: 0,
                        intensity: scale_u8(c.intensity, lamp.intensity_level_count as u32),
                    }
                }
            })
            .collect();

        let total_chunks = scaled.len().div_ceil(slot_count);

        for (chunk_idx, chunk) in scaled.chunks(slot_count).enumerate() {
            let is_last = chunk_idx == total_chunks - 1;
            let mut buf = vec![0u8; multi_size + 1];
            buf[0] = multi_report_id;

            // LampMultiUpdateReport layout (Section 26.11.1, MS reference Report 4):
            //   [ReportID, LampCount(8), LampUpdateFlags(8),
            //    LampIds[N](16-bit LE), RGBI[N](8-bit × 4)]
            buf[1] = chunk.len() as u8; // LampCount
            buf[2] = if is_last { 0x01 } else { 0x00 }; // LampUpdateFlags

            // Fill LampIds (16-bit LE each)
            let ids_start = 3;
            for (j, c) in chunk.iter().enumerate() {
                let off = ids_start + j * 2;
                let id_bytes = c.lamp_id.to_le_bytes();
                buf[off] = id_bytes[0];
                buf[off + 1] = id_bytes[1];
            }

            // Fill RGBI tuples (4 bytes each, starting after all LampId slots)
            let rgbi_start = ids_start + slot_count * 2;
            for (j, c) in chunk.iter().enumerate() {
                let off = rgbi_start + j * 4;
                buf[off] = c.red;
                buf[off + 1] = c.green;
                buf[off + 2] = c.blue;
                buf[off + 3] = c.intensity;
            }

            fd.feat_set(&buf)?;
        }

        Ok(())
    }

    /// One-line summary for CLI listing.
    ///
    /// Returns a static description based on descriptor info only — does not
    /// open the device or perform any ioctl calls.
    pub fn summary(&self) -> &'static str {
        let has_range = self.info.reports.range_update.is_some();
        let has_multi = self.info.reports.multi_update.is_some();
        match (has_range, has_multi) {
            (true, true) => "LampArray (range+multi update)",
            (true, false) => "LampArray (range update)",
            (false, true) => "LampArray (multi update)",
            (false, false) => "LampArray",
        }
    }
}

// --- LedRgbDevice ---

/// HID LED Page RGB LED (Usage Page 0x08, Section 11.7) control.
///
/// Uses the RGB LED collection (Usage 0x52) with individual channel controls:
///   - Red LED Channel (Usage 0x53)
///   - Blue LED Channel (Usage 0x54)  -- Note: spec order is R, B, G
///   - Green LED Channel (Usage 0x55)
///   - LED Intensity (Usage 0x56, optional)
///
/// Byte offsets are determined by descriptor parsing, not assumed.
pub struct LedRgbDevice<'a> {
    info: &'a LedRgbInfo,
}

impl<'a> LedRgbDevice<'a> {
    pub fn new(info: &'a LedRgbInfo) -> Self {
        Self { info }
    }

    /// The device path (e.g. `/dev/hidraw0`).
    pub fn path(&self) -> &str {
        &self.info.hidraw_path
    }

    /// The device name from sysfs.
    pub fn name(&self) -> &str {
        &self.info.name
    }

    /// Set RGB LED color.
    ///
    /// Maps arguments to the correct channel offsets parsed from the descriptor.
    /// Values are scaled from the caller's 0-255 range to the device's
    /// LogicalMaximum (e.g. 0-100 for intensity per Section 11.7).
    ///
    /// The report is sent via the appropriate mechanism for the report type
    /// parsed from the descriptor (Feature report via ioctl, Output report
    /// via write syscall).
    pub fn set_color(&self, r: u8, g: u8, b: u8, intensity: u8) -> Result<()> {
        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        let mut buf = vec![0u8; self.info.report_size + 1];
        buf[0] = self.info.report_id;

        // Scale color channels from 0-255 to each channel's LogicalMaximum.
        // When logical_max == 255 this is an identity transform.
        buf[1 + self.info.red_offset] = scale_u8(r, self.info.red_logical_max);
        buf[1 + self.info.blue_offset] = scale_u8(b, self.info.blue_logical_max);
        buf[1 + self.info.green_offset] = scale_u8(g, self.info.green_logical_max);

        if let Some(off) = self.info.intensity_offset {
            let int_max = self.info.intensity_logical_max.unwrap_or(255);
            buf[1 + off] = scale_u8(intensity, int_max);
        }

        match self.info.report_type {
            ReportType::Feature => fd.feat_set(&buf),
            ReportType::Output => fd.output_set(&buf),
            ReportType::Input => Err(Error::UnsupportedReportType),
        }
    }

    /// Return basic device info (LED Page has no LampArray-style attributes).
    pub fn get_attributes(&self) -> LedRgbAttributes {
        LedRgbAttributes {
            name: self.info.name.clone(),
            path: self.info.hidraw_path.clone(),
            protocol: "LED Page RGB (Usage Page 0x08, Section 11.7)",
            report_id: self.info.report_id,
            channel_size: self.info.channel_size,
            has_intensity: self.info.intensity_offset.is_some(),
        }
    }

    /// One-line summary for CLI listing.
    pub fn summary(&self) -> &'static str {
        "LED RGB"
    }
}
