//! Error types for hid-rgb-ctl.
//!
//! Hand-written error enum with `Display` and `Error` trait implementations.
//! Supports `?` operator via `From` impls for `std::io::Error` and `lexopt::Error`.

use std::fmt;

/// All errors that can occur in hid-rgb-ctl.
#[derive(Debug)]
pub enum Error {
    /// Permission denied accessing a hidraw device.
    PermissionDenied { path: String },
    /// The hidraw device path does not exist.
    DeviceNotFound { path: String },
    /// A required HID report type is missing from the device descriptor.
    MissingReport { report_name: String },
    /// LampAttributesResponseReport returned an unexpected LampId.
    LampIdMismatch { expected: u16, got: u16 },
    /// Caller specified a LampId that exceeds the device's LampCount.
    LampIdOutOfRange { lamp_id: u16, lamp_count: u16 },
    /// Caller specified the same LampId in multiple update slots.
    DuplicateLampId { lamp_id: u16 },
    /// The `auto` command was used on a non-LampArray device.
    NoAutonomousMode,
    /// The `set-lamp` command was used on a non-LampArray device.
    NoMultiUpdate,
    /// Device returned a report shorter than expected.
    TruncatedReport {
        report_name: &'static str,
        expected: usize,
        got: usize,
    },
    /// Attempted to write to an Input-only report (device-to-host).
    UnsupportedReportType,
    /// Invalid subcommand or argument.
    InvalidArgument(String),
    /// Wrapped I/O error.
    Io(std::io::Error),
    /// Wrapped lexopt argument parsing error.
    Arg(lexopt::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied { path } => {
                write!(f, "Permission denied on {path}")
            }
            Self::DeviceNotFound { path } => {
                write!(f, "Device not found: {path}")
            }
            Self::MissingReport { report_name } => {
                write!(f, "Device has no '{report_name}' report")
            }
            Self::LampIdMismatch { expected, got } => {
                write!(
                    f,
                    "LampAttributesResponse returned LampId {got}, expected {expected}"
                )
            }
            Self::LampIdOutOfRange {
                lamp_id,
                lamp_count,
            } => {
                write!(f, "LampId {lamp_id} exceeds device LampCount {lamp_count}")
            }
            Self::DuplicateLampId { lamp_id } => {
                write!(f, "Duplicate LampId {lamp_id} in update request")
            }
            Self::NoAutonomousMode => {
                write!(
                    f,
                    "This device does not support autonomous mode. \
                     Only LampArray (Usage Page 0x59) devices have this feature."
                )
            }
            Self::NoMultiUpdate => {
                write!(
                    f,
                    "Per-lamp color control requires a LampArray device (Usage Page 0x59) \
                     with a LampMultiUpdateReport."
                )
            }
            Self::TruncatedReport {
                report_name,
                expected,
                got,
            } => {
                write!(
                    f,
                    "Truncated {report_name} report: expected at least {expected} bytes, got {got}"
                )
            }
            Self::UnsupportedReportType => {
                write!(f, "Cannot write to an Input report (device-to-host only)")
            }
            Self::InvalidArgument(msg) => {
                write!(f, "{msg}")
            }
            Self::Io(e) => write!(f, "{e}"),
            Self::Arg(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Arg(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<lexopt::Error> for Error {
    fn from(e: lexopt::Error) -> Self {
        Self::Arg(e)
    }
}

/// Convenience type alias.
pub type Result<T> = std::result::Result<T, Error>;
