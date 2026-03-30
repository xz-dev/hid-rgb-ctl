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
//! use hid_rgb_ctl::{discover_devices, DeviceInfo, LampArrayDevice, LedRgbDevice};
//!
//! let devices = discover_devices();
//! for dev in &devices {
//!     match dev {
//!         DeviceInfo::LampArray(info) => {
//!             let device = LampArrayDevice::new(info);
//!             device.set_color(255, 0, 0, 255).unwrap();
//!         }
//!         DeviceInfo::LedRgb(info) => {
//!             let device = LedRgbDevice::new(info);
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

pub use descriptor::{discover_devices, DeviceInfo, LampArrayInfo, LedRgbInfo};
pub use device::{LampArrayDevice, LedRgbDevice};
pub use error::Error;
