//! Control RGB lighting on HID LampArray and LED Page devices on Linux.
//!
//! This crate provides:
//! - Auto-discovery of HID RGB devices by parsing report descriptors from sysfs
//! - Support for HID LampArray (Usage Page 0x59) and LED Page RGB (Usage Page 0x08)
//! - No hardcoded vendor/product IDs — works with any compliant device
//!
//! # Example
//!
//! ```no_run
//! use hid_rgb_ctl::{discover_devices, DeviceKind, LampArrayDevice, LedRgbDevice};
//!
//! let devices = discover_devices();
//! for dev in &devices {
//!     match &dev.kind {
//!         DeviceKind::LampArray(_) => {
//!             let device = LampArrayDevice::new(dev);
//!             device.set_color(255, 0, 0, 255).unwrap();
//!         }
//!         DeviceKind::LedRgb(_) => {
//!             let device = LedRgbDevice::new(dev);
//!             device.set_color(255, 0, 0, 255).unwrap();
//!         }
//!     }
//! }
//! ```

#[doc(hidden)]
pub mod cli;
pub mod descriptor;
pub mod device;
pub mod error;

pub use descriptor::{
    discover_device, discover_devices, DeviceInfo, DeviceKind, LampArrayReports, LedRgbChannelInfo,
    ReportInfo, ReportType,
};
pub use device::{LampArrayAttributes, LampArrayDevice, LampAttributes, LampColor, LedRgbDevice};
pub use error::Error;
