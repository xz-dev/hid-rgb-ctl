//! HID RGB device control.
//!
//! Provides two device types:
//! - [`LampArrayDevice`]: HID LampArray (Usage Page 0x59) per HUT v1.4 Section 26
//! - [`LedRgbDevice`]: LED Page RGB LED (Usage Page 0x08) per HUT v1.4 Section 11.7

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;

use crate::descriptor::{LampArrayInfo, LedRgbInfo, ReportInfo};
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
        _ => "Unknown",
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

/// LampArray device attributes (Section 26.2).
#[derive(Debug, Clone)]
pub struct LampArrayAttributes {
    pub lamp_count: u16,
    /// Bounding box in micrometers.
    pub width_um: u32,
    pub height_um: u32,
    pub depth_um: u32,
    pub kind: u32,
    pub kind_name: String,
    pub min_update_interval_us: u32,
}

/// LED Page RGB device attributes (Section 11.7).
#[derive(Debug, Clone)]
pub struct LedRgbAttributes {
    pub name: String,
    pub path: String,
    pub protocol: String,
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
        let fd = OpenOptions::new().read(true).write(true).open(path)?;
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
        Ok(buf)
    }

    /// Write a HID Feature report (HIDIOCSFEATURE).
    fn feat_set(&self, buf: &mut [u8]) -> Result<()> {
        let ret =
            unsafe { libc::ioctl(self.fd.as_raw_fd(), hidiocsfeature(buf.len()), buf.as_ptr()) };
        if ret < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

// --- Helper to require a report ---

fn require_report<'a>(
    reports: &'a HashMap<String, ReportInfo>,
    name: &str,
) -> Result<&'a ReportInfo> {
    reports.get(name).ok_or_else(|| Error::MissingReport {
        report_name: name.to_string(),
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
        let rinfo = require_report(&self.info.reports, "attributes")?;
        let buf = fd.feat_get(rinfo.report_id, rinfo.size)?;

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
            kind_name: lamp_array_kind_name(kind).to_string(),
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
        let req_info = require_report(&self.info.reports, "attr_request")?;
        let mut req_buf = vec![0u8; req_info.size + 1];
        req_buf[0] = req_info.report_id;
        let idx_bytes = index.to_le_bytes();
        req_buf[1] = idx_bytes[0];
        req_buf[2] = idx_bytes[1];
        fd.feat_set(&mut req_buf)?;

        let resp_info = require_report(&self.info.reports, "attr_response")?;
        let buf = fd.feat_get(resp_info.report_id, resp_info.size)?;

        // Layout: [ReportID, LampId(16), PosX(32), PosY(32), PosZ(32),
        //          Latency(32), Purposes(32), RedCount(8), GreenCount(8),
        //          BlueCount(8), IntensityCount(8), IsProgrammable(8),
        //          InputBinding(8)]
        let lamp_id = u16::from_le_bytes([buf[1], buf[2]]);
        let pos_x = u32::from_le_bytes([buf[3], buf[4], buf[5], buf[6]]);
        let pos_y = u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]);
        let pos_z = u32::from_le_bytes([buf[11], buf[12], buf[13], buf[14]]);
        let latency = u32::from_le_bytes([buf[15], buf[16], buf[17], buf[18]]);
        let purposes = u32::from_le_bytes([buf[19], buf[20], buf[21], buf[22]]);
        let r_cnt = buf[23];
        let g_cnt = buf[24];
        let b_cnt = buf[25];
        let i_cnt = buf[26];
        let prog = buf[27];
        let binding = buf[28];

        Ok(LampAttributes {
            lamp_id,
            position_x_um: pos_x,
            position_y_um: pos_y,
            position_z_um: pos_z,
            update_latency_us: latency,
            lamp_purposes: purposes,
            red_level_count: r_cnt,
            green_level_count: g_cnt,
            blue_level_count: b_cnt,
            intensity_level_count: i_cnt,
            is_programmable: prog != 0,
            input_binding: binding,
        })
    }

    /// Read attributes and all lamp info using a single fd.
    ///
    /// More efficient than calling `get_attributes()` + `get_lamp()` in a loop,
    /// which opens a new fd for each call.
    pub fn get_attributes_and_lamps(
        &self,
    ) -> Result<(LampArrayAttributes, Vec<Result<LampAttributes>>)> {
        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        let attrs = self.get_attributes_with_fd(&fd)?;
        let mut lamps = Vec::with_capacity(attrs.lamp_count as usize);
        for i in 0..attrs.lamp_count {
            lamps.push(self.get_lamp_with_fd(&fd, i));
        }
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
        let ctrl_info = require_report(&self.info.reports, "control")?;
        let mut buf = vec![0u8; ctrl_info.size + 1];
        buf[0] = ctrl_info.report_id;
        buf[1] = if enabled { 0x01 } else { 0x00 };
        fd.feat_set(&mut buf)
    }

    /// Set all lamps to a uniform color.
    ///
    /// Disables autonomous mode, then sends a LampRangeUpdate covering
    /// all lamps (LampIdStart=0, LampIdEnd=LampCount-1) with
    /// LampUpdateComplete=1 to apply immediately.
    ///
    /// Opens the fd once for the entire sequence (autonomous + attrs + update).
    pub fn set_color(&self, r: u8, g: u8, b: u8, intensity: u8) -> Result<()> {
        let range_info = require_report(&self.info.reports, "range_update")?;
        let range_report_id = range_info.report_id;
        let range_size = range_info.size;

        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        self.set_autonomous_with_fd(&fd, false)?;

        let lamp_end = match self.get_attributes_with_fd(&fd) {
            Ok(attrs) => {
                if attrs.lamp_count > 0 {
                    attrs.lamp_count - 1
                } else {
                    0
                }
            }
            Err(e) => {
                eprintln!("Warning: Could not read lamp count, defaulting to 0: {e}");
                0
            }
        };

        // LampRangeUpdateReport layout (Section 26.4):
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
        buf[6] = r;
        buf[7] = g;
        buf[8] = b;
        buf[9] = intensity;

        fd.feat_set(&mut buf)
    }

    /// One-line summary for CLI listing.
    pub fn summary(&self) -> String {
        match self.get_attributes() {
            Ok(attrs) => format!(
                "LampArray  {} lamp(s), {}",
                attrs.lamp_count, attrs.kind_name
            ),
            Err(_) => "LampArray".to_string(),
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

    /// Set RGB LED color via Feature report.
    ///
    /// Maps arguments to the correct channel offsets parsed from the descriptor.
    /// Note the spec channel order is R(0x53), B(0x54), G(0x55) -- we handle
    /// the mapping internally so callers always pass (r, g, b).
    pub fn set_color(&self, r: u8, g: u8, b: u8, intensity: u8) -> Result<()> {
        let fd = HidrawFd::open(&self.info.hidraw_path)?;
        let mut buf = vec![0u8; self.info.report_size + 1];
        buf[0] = self.info.report_id;
        buf[1 + self.info.red_offset] = r;
        buf[1 + self.info.blue_offset] = b;
        buf[1 + self.info.green_offset] = g;
        if let Some(off) = self.info.intensity_offset {
            buf[1 + off] = intensity;
        }
        fd.feat_set(&mut buf)
    }

    /// Return basic device info (LED Page has no LampArray-style attributes).
    pub fn get_attributes(&self) -> LedRgbAttributes {
        LedRgbAttributes {
            name: self.info.name.clone(),
            path: self.info.hidraw_path.clone(),
            protocol: "LED Page RGB (Usage Page 0x08, Section 11.7)".to_string(),
            report_id: self.info.report_id,
            channel_size: self.info.channel_size,
            has_intensity: self.info.intensity_offset.is_some(),
        }
    }

    /// One-line summary for CLI listing.
    pub fn summary(&self) -> String {
        "LED RGB".to_string()
    }
}
