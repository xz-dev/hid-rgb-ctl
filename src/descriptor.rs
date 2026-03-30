//! HID report descriptor parser and device discovery.
//!
//! Parses binary HID report descriptors to find devices implementing:
//! - Usage Page 0x59 (Lighting and Illumination) -- HID LampArray protocol
//! - Usage Page 0x08 (LED Page) with Usage 0x52 (RGB LED) -- Legacy RGB LED
//!
//! Reference: USB HID Usage Tables v1.4
//!   - Section 26: Lighting and Illumination Page (0x59)
//!   - Section 11.7: Multicolor (RGB) LED on LED Page (0x08)

use std::collections::HashMap;
use std::fs;
use std::path::Path;

// --- Usage Page constants ---

const USAGE_PAGE_LED: u32 = 0x08;
const USAGE_PAGE_LIGHTING: u32 = 0x59;

// Usage IDs for Lighting and Illumination Page (0x59), Section 26
const USAGE_LAMP_ARRAY: u32 = 0x01; // LampArray application collection
const USAGE_LAMP_ARRAY_ATTRIBUTES_REPORT: u32 = 0x02;
const USAGE_LAMP_ATTR_REQUEST_REPORT: u32 = 0x20;
const USAGE_LAMP_ATTR_RESPONSE_REPORT: u32 = 0x22;
const USAGE_LAMP_MULTI_UPDATE_REPORT: u32 = 0x50;
const USAGE_LAMP_RANGE_UPDATE_REPORT: u32 = 0x60;
const USAGE_LAMP_ARRAY_CONTROL_REPORT: u32 = 0x70;

// Usage IDs for LED Page (0x08), Section 11.7
const USAGE_RGB_LED: u32 = 0x52;
const USAGE_RED_LED_CHANNEL: u32 = 0x53;
const USAGE_BLUE_LED_CHANNEL: u32 = 0x54; // Note: Blue before Green in spec
const USAGE_GREEN_LED_CHANNEL: u32 = 0x55;
const USAGE_LED_INTENSITY: u32 = 0x56;

// HID item tags (prefix byte with size bits masked out)
// Global items
const TAG_USAGE_PAGE: u8 = 0x04;
const TAG_LOGICAL_MIN: u8 = 0x14;
const TAG_LOGICAL_MAX: u8 = 0x24;
const TAG_REPORT_SIZE: u8 = 0x74;
const TAG_REPORT_ID: u8 = 0x84;
const TAG_REPORT_COUNT: u8 = 0x94;

// Local items
const TAG_USAGE: u8 = 0x08;
const TAG_USAGE_MIN: u8 = 0x18;
const TAG_USAGE_MAX: u8 = 0x28;

// Main items
const TAG_INPUT: u8 = 0x80;
const TAG_OUTPUT: u8 = 0x90;
const TAG_FEATURE: u8 = 0xB0;
const TAG_COLLECTION: u8 = 0xA0;
const TAG_END_COLLECTION: u8 = 0xC0;

// Collection types
const COLLECTION_APPLICATION: u32 = 0x01;
const COLLECTION_LOGICAL: u32 = 0x02;

// --- HID report type ---

/// The type of a HID report (Feature, Output, or Input).
///
/// Determines how the report is communicated on Linux HIDRAW:
/// - Feature: via ioctl (HIDIOCSFEATURE / HIDIOCGFEATURE)
/// - Output: via write() syscall
/// - Input: via read() syscall
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportType {
    Feature,
    Output,
    Input,
}

/// Map from LampArray collection usage to report name.
fn lamp_array_report_name(usage: u32) -> Option<&'static str> {
    match usage {
        USAGE_LAMP_ARRAY_ATTRIBUTES_REPORT => Some("attributes"),
        USAGE_LAMP_ATTR_REQUEST_REPORT => Some("attr_request"),
        USAGE_LAMP_ATTR_RESPONSE_REPORT => Some("attr_response"),
        USAGE_LAMP_MULTI_UPDATE_REPORT => Some("multi_update"),
        USAGE_LAMP_RANGE_UPDATE_REPORT => Some("range_update"),
        USAGE_LAMP_ARRAY_CONTROL_REPORT => Some("control"),
        _ => None,
    }
}

/// Check if a tag is a main data tag (Input, Output, or Feature).
fn is_main_data_tag(tag: u8) -> bool {
    matches!(tag, TAG_INPUT | TAG_OUTPUT | TAG_FEATURE)
}

// --- Public types ---

/// Parsed HID report metadata.
#[derive(Debug, Clone)]
pub struct ReportInfo {
    pub report_id: u8,
    /// Total data bytes (excluding report ID byte).
    pub size: usize,
}

/// Device implementing HID LampArray (Usage Page 0x59).
///
/// Report IDs and sizes are parsed from the device's HID report descriptor,
/// not hardcoded, so this works with any compliant LampArray device.
#[derive(Debug, Clone)]
pub struct LampArrayInfo {
    pub hidraw_path: String,
    pub name: String,
    /// Keys: "attributes", "attr_request", "attr_response",
    ///        "multi_update", "range_update", "control"
    pub reports: HashMap<String, ReportInfo>,
}

/// Device implementing LED Page RGB LED (Usage Page 0x08, Section 11.7).
///
/// The RGB LED collection (Usage 0x52) contains:
///   - Red LED Channel (Usage 0x53)
///   - Blue LED Channel (Usage 0x54)  -- Note: Blue before Green per spec
///   - Green LED Channel (Usage 0x55)
///   - LED Intensity (Usage 0x56, optional)
///
/// Byte offsets are parsed from the descriptor to handle any report layout.
#[derive(Debug, Clone)]
pub struct LedRgbInfo {
    pub hidraw_path: String,
    pub name: String,
    pub report_id: u8,
    /// Total data bytes (excluding report ID).
    pub report_size: usize,
    /// Byte offset of Red channel within report data.
    pub red_offset: usize,
    /// Byte offset of Blue channel.
    pub blue_offset: usize,
    /// Byte offset of Green channel.
    pub green_offset: usize,
    /// Byte offset of Intensity (None if absent).
    pub intensity_offset: Option<usize>,
    /// Bits per channel (typically 8).
    pub channel_size: u32,
    /// The HID report type (Feature or Output).
    pub report_type: ReportType,
    /// LogicalMaximum per color channel from the descriptor (typically 255).
    pub red_logical_max: u32,
    pub green_logical_max: u32,
    pub blue_logical_max: u32,
    /// LogicalMaximum of the Intensity channel (spec recommends 100).
    /// None if the device has no intensity channel.
    pub intensity_logical_max: Option<u32>,
}

/// Either a LampArray or LED RGB device info.
#[derive(Debug, Clone)]
pub enum DeviceInfo {
    LampArray(LampArrayInfo),
    LedRgb(LedRgbInfo),
}

impl DeviceInfo {
    /// The hidraw device path (e.g. `/dev/hidraw0`).
    pub fn hidraw_path(&self) -> &str {
        match self {
            Self::LampArray(info) => &info.hidraw_path,
            Self::LedRgb(info) => &info.hidraw_path,
        }
    }

    /// The device name from sysfs.
    pub fn name(&self) -> &str {
        match self {
            Self::LampArray(info) => &info.name,
            Self::LedRgb(info) => &info.name,
        }
    }
}

// --- HID Report Descriptor Parser ---

/// Accumulates LED RGB channel offsets during descriptor parsing.
#[derive(Debug)]
struct LedRgbChannelBuilder {
    report_id: u8,
    report_size: usize,
    red_offset: Option<usize>,
    blue_offset: Option<usize>,
    green_offset: Option<usize>,
    intensity_offset: Option<usize>,
    channel_size: u32,
    /// The HID report type (Feature, Output, or Input) for this RGB LED.
    report_type: ReportType,
    /// LogicalMaximum per color channel (typically 255 for all).
    red_logical_max: u32,
    green_logical_max: u32,
    blue_logical_max: u32,
    /// LogicalMaximum of the Intensity channel (spec recommends 100).
    intensity_logical_max: Option<u32>,
}

impl Default for LedRgbChannelBuilder {
    fn default() -> Self {
        Self {
            report_id: 0,
            report_size: 0,
            red_offset: None,
            blue_offset: None,
            green_offset: None,
            intensity_offset: None,
            channel_size: 8, // Typical default per HID spec
            report_type: ReportType::Feature,
            red_logical_max: 255,
            green_logical_max: 255,
            blue_logical_max: 255,
            intensity_logical_max: None,
        }
    }
}

impl LedRgbChannelBuilder {
    /// True when all mandatory channels (R, G, B) have been found.
    fn is_complete(&self) -> bool {
        self.red_offset.is_some() && self.blue_offset.is_some() && self.green_offset.is_some()
    }
}

/// Usage entry stored during parsing -- either a plain usage ID
/// or a pending min value awaiting a USAGE_MAX to expand the range.
#[derive(Debug, Clone)]
enum UsageEntry {
    Single(u32),
    Min(u32),
}

/// Mutable state for the HID descriptor parser.
struct ParserState {
    // Global items (persist across Main items)
    usage_page: u32,
    report_id: u8,
    report_size: u32,
    report_count: u32,
    logical_min: i32,
    logical_max: i32,

    // Local items (reset after each Main item)
    usages: Vec<UsageEntry>,

    // Accumulated results
    lamp_array_reports: HashMap<String, ReportInfo>,
    led_rgb_channels: HashMap<u8, LedRgbChannelBuilder>,

    // Per-report accumulated data bits for final size calculation
    report_data_bits: HashMap<u8, u32>,

    // Collection / context tracking
    collection_depth: u32,
    current_lighting_report_name: Option<String>,
    in_rgb_led_collection: bool,
    /// Whether we are inside a LampArray Application collection (Usage Page 0x59, Usage 0x01).
    in_lamp_array_app: bool,
}

impl ParserState {
    fn new() -> Self {
        Self {
            usage_page: 0,
            report_id: 0,
            report_size: 0,
            report_count: 0,
            logical_min: 0,
            logical_max: 0,
            usages: Vec::new(),
            lamp_array_reports: HashMap::new(),
            led_rgb_channels: HashMap::new(),
            report_data_bits: HashMap::new(),
            collection_depth: 0,
            current_lighting_report_name: None,
            in_rgb_led_collection: false,
            in_lamp_array_app: false,
        }
    }

    // --- Global item handlers ---

    fn handle_global(&mut self, tag: u8, val: u32, payload: &[u8]) {
        match tag {
            TAG_USAGE_PAGE => self.usage_page = val,
            TAG_LOGICAL_MIN => self.logical_min = payload_value_signed(payload),
            TAG_LOGICAL_MAX => self.logical_max = payload_value_signed(payload),
            TAG_REPORT_ID => self.report_id = val as u8,
            TAG_REPORT_SIZE => self.report_size = val,
            TAG_REPORT_COUNT => self.report_count = val,
            _ => {}
        }
    }

    // --- Local item handlers ---

    fn handle_local(&mut self, tag: u8, val: u32) {
        match tag {
            TAG_USAGE => {
                self.usages.push(UsageEntry::Single(val));
            }
            TAG_USAGE_MIN => {
                self.usages.push(UsageEntry::Min(val));
            }
            TAG_USAGE_MAX => {
                // Expand usage range from the last USAGE_MIN
                if let Some(UsageEntry::Min(umin)) = self.usages.last().cloned() {
                    self.usages.pop();
                    for u in umin..=val {
                        self.usages.push(UsageEntry::Single(u));
                    }
                }
            }
            _ => {}
        }
    }

    // --- Main item handlers ---

    fn handle_main(&mut self, tag: u8, val: u32) {
        match tag {
            TAG_COLLECTION => self.on_collection(val),
            TAG_END_COLLECTION => self.on_end_collection(),
            t if is_main_data_tag(t) => self.on_data_item(t),
            _ => {}
        }
    }

    fn on_collection(&mut self, collection_type: u32) {
        self.collection_depth += 1;

        // LampArray Application collection (Usage Page 0x59, Usage 0x01)
        if self.usage_page == USAGE_PAGE_LIGHTING && collection_type == COLLECTION_APPLICATION {
            for entry in &self.usages {
                if let UsageEntry::Single(usage) = entry {
                    if *usage == USAGE_LAMP_ARRAY {
                        self.in_lamp_array_app = true;
                    }
                }
            }
        }

        // LampArray sub-report collections: only match inside a LampArray app collection
        if self.usage_page == USAGE_PAGE_LIGHTING && self.in_lamp_array_app {
            for entry in &self.usages {
                if let UsageEntry::Single(usage) = entry {
                    if let Some(name) = lamp_array_report_name(*usage) {
                        self.current_lighting_report_name = Some(name.to_string());
                    }
                }
            }
        }

        // RGB LED collection: per Section 11.7, RGB LED is a Logical collection (CL)
        if self.usage_page == USAGE_PAGE_LED && collection_type == COLLECTION_LOGICAL {
            for entry in &self.usages {
                if let UsageEntry::Single(usage) = entry {
                    if *usage == USAGE_RGB_LED {
                        self.in_rgb_led_collection = true;
                        self.led_rgb_channels.entry(self.report_id).or_default();
                    }
                }
            }
        }

        self.usages.clear();
    }

    fn on_end_collection(&mut self) {
        self.collection_depth = self.collection_depth.saturating_sub(1);
        if self.collection_depth <= 1 {
            self.current_lighting_report_name = None;
            self.in_rgb_led_collection = false;
        }
        if self.collection_depth == 0 {
            self.in_lamp_array_app = false;
        }
        self.usages.clear();
    }

    fn on_data_item(&mut self, tag: u8) {
        let total_bits = self.report_size * self.report_count;
        let rid = self.report_id;

        // Capture absolute bit offset before this item for channel positioning
        let bit_offset_before = self.report_data_bits.get(&rid).copied().unwrap_or(0);
        *self.report_data_bits.entry(rid).or_insert(0) += total_bits;

        // Determine report type from the Main item tag
        let report_type = match tag {
            TAG_OUTPUT => ReportType::Output,
            TAG_INPUT => ReportType::Input,
            _ => ReportType::Feature,
        };

        // Lighting Page (0x59): record report info
        if self.usage_page == USAGE_PAGE_LIGHTING {
            if let Some(ref name) = self.current_lighting_report_name {
                self.lamp_array_reports
                    .entry(name.clone())
                    .or_insert(ReportInfo {
                        report_id: rid,
                        size: 0,
                    });
            }
        }

        // LED Page (0x08): record channel byte offsets (absolute within report)
        if self.usage_page == USAGE_PAGE_LED && self.in_rgb_led_collection {
            let logical_max = self.logical_max;
            let builder = self.led_rgb_channels.entry(rid).or_default();
            builder.report_type = report_type;

            let lmax = logical_max.max(0) as u32;
            for (i, entry) in self.usages.iter().enumerate() {
                if let UsageEntry::Single(usage) = entry {
                    let byte_off = ((bit_offset_before + i as u32 * self.report_size) / 8) as usize;
                    match *usage {
                        USAGE_RED_LED_CHANNEL => {
                            builder.red_offset = Some(byte_off);
                            builder.channel_size = self.report_size;
                            builder.red_logical_max = lmax;
                        }
                        USAGE_BLUE_LED_CHANNEL => {
                            builder.blue_offset = Some(byte_off);
                            builder.blue_logical_max = lmax;
                        }
                        USAGE_GREEN_LED_CHANNEL => {
                            builder.green_offset = Some(byte_off);
                            builder.green_logical_max = lmax;
                        }
                        USAGE_LED_INTENSITY => {
                            builder.intensity_offset = Some(byte_off);
                            builder.intensity_logical_max = Some(lmax);
                        }
                        _ => {}
                    }
                }
            }
        }

        self.usages.clear();
    }

    // --- Finalize ---

    fn finalize(mut self) -> (HashMap<String, ReportInfo>, Vec<LedRgbChannelBuilder>) {
        // Fill in byte sizes for LampArray reports
        for rinfo in self.lamp_array_reports.values_mut() {
            let bits = self
                .report_data_bits
                .get(&rinfo.report_id)
                .copied()
                .unwrap_or(0);
            rinfo.size = bits.div_ceil(8) as usize;
        }

        // Collect complete LED RGB channel builders with computed sizes
        let complete = self
            .led_rgb_channels
            .into_iter()
            .filter_map(|(rid, mut builder)| {
                if builder.is_complete() {
                    builder.report_id = rid;
                    let bits = self.report_data_bits.get(&rid).copied().unwrap_or(0);
                    builder.report_size = bits.div_ceil(8) as usize;
                    Some(builder)
                } else {
                    None
                }
            })
            .collect();

        (self.lamp_array_reports, complete)
    }
}

/// Parse one HID descriptor item.
///
/// Returns (tag, item_type, payload_slice, total_item_bytes).
/// `item_type`: 0=Main, 1=Global, 2=Local, 3=reserved/long (skip).
///
/// Handles both short items (HID 1.11 §6.2.2.2) and long items (§6.2.2.3).
fn parse_item(data: &[u8], offset: usize) -> Option<(u8, u8, &[u8], usize)> {
    if offset >= data.len() {
        return None;
    }

    let prefix = data[offset];

    // Long item: prefix == 0xFE (HID 1.11 §6.2.2.3)
    // Format: 0xFE, bDataSize, bLongItemTag, data[bDataSize]
    if prefix == 0xFE {
        if offset + 2 >= data.len() {
            return None;
        }
        let data_size = data[offset + 1] as usize;
        let total = 3 + data_size;
        if offset + total > data.len() {
            return None;
        }
        // Long items have no standard tags; skip with item_type=3 (reserved)
        return Some((0xFE, 3, &[], total));
    }

    let mut size = (prefix & 0x03) as usize;
    if size == 3 {
        size = 4; // Size code 3 means 4 bytes
    }
    let tag = prefix & 0xFC;
    let item_type = (prefix >> 2) & 0x03; // 0=Main, 1=Global, 2=Local

    let end = offset + 1 + size;
    if end > data.len() {
        return None;
    }

    let payload = &data[offset + 1..end];
    Some((tag, item_type, payload, 1 + size))
}

/// Decode a HID item payload as a signed integer (sign-extended).
///
/// HID 1.11 §6.2.2.7 requires LogicalMinimum/LogicalMaximum to be
/// interpreted as signed values with sign extension based on payload size.
fn payload_value_signed(payload: &[u8]) -> i32 {
    match payload.len() {
        0 => 0,
        1 => payload[0] as i8 as i32,
        2 => i16::from_le_bytes([payload[0], payload[1]]) as i32,
        4 => i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
        _ => {
            // Fallback: little-endian decode up to 4 bytes, then sign-extend
            let mut val = 0u32;
            let len = payload.len().min(4);
            for (i, &b) in payload.iter().enumerate().take(len) {
                val |= (b as u32) << (8 * i);
            }
            // Sign-extend from the actual byte width
            let shift = 32 - (len * 8) as u32;
            ((val << shift) as i32) >> shift
        }
    }
}

/// Decode a HID item payload as an unsigned integer.
fn payload_value(payload: &[u8]) -> u32 {
    match payload.len() {
        0 => 0,
        1 => payload[0] as u32,
        2 => u16::from_le_bytes([payload[0], payload[1]]) as u32,
        4 => u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
        _ => {
            // Fallback: little-endian decode up to 4 bytes
            let mut val = 0u32;
            for (i, &b) in payload.iter().enumerate().take(4) {
                val |= (b as u32) << (8 * i);
            }
            val
        }
    }
}

/// Parse a binary HID report descriptor.
///
/// Returns:
///   - lamp_array_reports: map of report name -> ReportInfo for Lighting Page (0x59)
///   - led_rgb_builders: list of LedRgbChannelBuilder for LED Page (0x08) RGB LED
fn parse_descriptor(desc: &[u8]) -> (HashMap<String, ReportInfo>, Vec<LedRgbChannelBuilder>) {
    let mut state = ParserState::new();
    let mut offset = 0;

    while offset < desc.len() {
        let Some((tag, item_type, payload, item_size)) = parse_item(desc, offset) else {
            break;
        };
        offset += item_size;
        let val = payload_value(payload);

        match item_type {
            1 => state.handle_global(tag, val, payload), // Global
            2 => state.handle_local(tag, val),  // Local
            0 => state.handle_main(tag, val), // Main (val is collection type for Collection items)
            _ => {}
        }
    }

    state.finalize()
}

// --- Device Discovery ---

/// Read HID_NAME from the device's uevent file.
fn get_hid_name(hidraw: &str) -> String {
    let uevent_path = format!("/sys/class/hidraw/{hidraw}/device/uevent");
    if let Ok(content) = fs::read_to_string(&uevent_path) {
        for line in content.lines() {
            if let Some(name) = line.strip_prefix("HID_NAME=") {
                return name.to_string();
            }
        }
    }
    "Unknown".to_string()
}

/// Scan all hidraw devices for LampArray and LED RGB support.
///
/// Reads each device's HID report descriptor from sysfs and parses it
/// to find devices implementing:
/// - Usage Page 0x59 (Lighting and Illumination) -- LampArray
/// - Usage Page 0x08 (LED Page) with Usage 0x52 (RGB LED)
///
/// Returns a list of [`DeviceInfo`] objects.
pub fn discover_devices() -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    let hidraw_dir = Path::new("/sys/class/hidraw");

    if !hidraw_dir.exists() {
        return devices;
    }

    let mut entries: Vec<_> = match fs::read_dir(hidraw_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return devices,
    };
    entries.sort();

    for entry in entries {
        let desc_path = entry.join("device").join("report_descriptor");
        if !desc_path.exists() {
            continue;
        }

        let desc_bytes = match fs::read(&desc_path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        if desc_bytes.is_empty() {
            continue;
        }

        let hidraw_name = match entry.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };

        let (lamp_reports, led_rgb_builders) = parse_descriptor(&desc_bytes);

        // Check for LampArray (Usage Page 0x59)
        if !lamp_reports.is_empty()
            && lamp_reports.contains_key("range_update")
            && lamp_reports.contains_key("control")
        {
            devices.push(DeviceInfo::LampArray(LampArrayInfo {
                hidraw_path: format!("/dev/{hidraw_name}"),
                name: get_hid_name(&hidraw_name),
                reports: lamp_reports,
            }));
        }

        // Check for LED Page RGB LED (Usage Page 0x08)
        // Builders are already filtered to is_complete() by finalize()
        for builder in led_rgb_builders {
            devices.push(DeviceInfo::LedRgb(LedRgbInfo {
                hidraw_path: format!("/dev/{hidraw_name}"),
                name: get_hid_name(&hidraw_name),
                report_id: builder.report_id,
                report_size: builder.report_size,
                red_offset: builder.red_offset.unwrap(),
                blue_offset: builder.blue_offset.unwrap(),
                green_offset: builder.green_offset.unwrap(),
                intensity_offset: builder.intensity_offset,
                channel_size: builder.channel_size,
                report_type: builder.report_type,
                red_logical_max: builder.red_logical_max,
                green_logical_max: builder.green_logical_max,
                blue_logical_max: builder.blue_logical_max,
                intensity_logical_max: builder.intensity_logical_max,
            }));
        }
    }

    devices
}
