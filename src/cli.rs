//! Command-line interface for hid-rgb-ctl.
//!
//! Uses `lexopt` for manual argument parsing with full control over
//! help text and error messages.

use lexopt::prelude::*;

use crate::descriptor::DeviceInfo;
use crate::device::{LampArrayDevice, LedRgbDevice};
use crate::error::Error;

// --- Color presets ---

const PRESETS: &[(&str, (u8, u8, u8))] = &[
    ("red", (255, 0, 0)),
    ("green", (0, 255, 0)),
    ("blue", (0, 0, 255)),
    ("white", (255, 255, 255)),
    ("cyan", (0, 255, 255)),
    ("yellow", (255, 255, 0)),
    ("orange", (255, 165, 0)),
    ("purple", (128, 0, 255)),
    ("pink", (255, 105, 180)),
    ("off", (0, 0, 0)),
];

fn preset_names() -> String {
    PRESETS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn find_preset(name: &str) -> Option<(u8, u8, u8)> {
    let lower = name.to_lowercase();
    PRESETS
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, rgb)| *rgb)
}

// --- Color parsing ---

/// Parse color from CLI arguments.
///
/// Accepts:
///   - Preset name: "red", "blue", etc.
///   - Three decimal values: "255" "100" "0"
///   - Six-character hex string: "ff6400" or "#ff6400"
fn parse_color(args: &[String]) -> Option<(u8, u8, u8)> {
    if args.len() == 1 {
        let name = &args[0];

        // Try preset
        if let Some(rgb) = find_preset(name) {
            return Some(rgb);
        }

        // Try hex (is_ascii guard prevents panic on multi-byte UTF-8 indexing)
        let s = name.strip_prefix('#').unwrap_or(name);
        if s.len() == 6 && s.is_ascii() {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            return Some((r, g, b));
        }
    } else if args.len() == 3 {
        let r: u8 = args[0].parse().ok()?;
        let g: u8 = args[1].parse().ok()?;
        let b: u8 = args[2].parse().ok()?;
        return Some((r, g, b));
    }
    None
}

// --- Device helpers ---

fn find_device<'a>(devices: &'a [DeviceInfo], path: Option<&str>) -> Option<&'a DeviceInfo> {
    if devices.is_empty() {
        return None;
    }
    match path {
        None => Some(&devices[0]),
        Some(p) => devices.iter().find(|d| d.hidraw_path() == p),
    }
}

// --- Subcommands ---

fn cmd_list(devices: &[DeviceInfo]) {
    if devices.is_empty() {
        println!("No HID RGB devices found.");
        return;
    }

    for d in devices {
        let summary = match d {
            DeviceInfo::LampArray(info) => LampArrayDevice::new(info).summary(),
            DeviceInfo::LedRgb(info) => LedRgbDevice::new(info).summary(),
        };
        println!("{}  {}  {}", d.hidraw_path(), d.name(), summary);
    }
}

fn cmd_get(info: &DeviceInfo) {
    match info {
        DeviceInfo::LampArray(la_info) => {
            let dev = LampArrayDevice::new(la_info);
            match dev.get_attributes_and_lamps() {
                Ok((attrs, lamps)) => {
                    println!("Device: {}", dev.name());
                    println!("Protocol: HID LampArray (Usage Page 0x59)");
                    println!("Path: {}", dev.path());
                    println!("Lamps: {}", attrs.lamp_count);
                    println!("Kind: {}", attrs.kind_name);
                    println!(
                        "Bounding box: {:.1} x {:.1} x {:.1} mm",
                        attrs.width_um as f64 / 1000.0,
                        attrs.height_um as f64 / 1000.0,
                        attrs.depth_um as f64 / 1000.0
                    );
                    println!("Min update interval: {} us", attrs.min_update_interval_us);

                    for lamp in &lamps {
                        println!("\nLamp {}:", lamp.lamp_id);
                        println!(
                            "  Position: ({:.1}, {:.1}, {:.1}) mm",
                            lamp.position_x_um as f64 / 1000.0,
                            lamp.position_y_um as f64 / 1000.0,
                            lamp.position_z_um as f64 / 1000.0
                        );
                        println!(
                            "  RGB levels: {}/{}/{}",
                            lamp.red_level_count, lamp.green_level_count, lamp.blue_level_count
                        );
                        println!("  Intensity levels: {}", lamp.intensity_level_count);
                        println!(
                            "  Programmable: {}",
                            if lamp.is_programmable { "yes" } else { "no" }
                        );
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        DeviceInfo::LedRgb(rgb_info) => {
            let dev = LedRgbDevice::new(rgb_info);
            let attrs = dev.get_attributes();
            println!("Device: {}", attrs.name);
            println!("Protocol: {}", attrs.protocol);
            println!("Path: {}", attrs.path);
            println!("Report ID: 0x{:02x}", attrs.report_id);
            println!("Channel size: {} bits", attrs.channel_size);
            println!(
                "Has intensity: {}",
                if attrs.has_intensity { "yes" } else { "no" }
            );
        }
    }
}

fn cmd_set(info: &DeviceInfo, r: u8, g: u8, b: u8, intensity: u8) -> Result<(), Error> {
    match info {
        DeviceInfo::LampArray(la_info) => {
            let dev = LampArrayDevice::new(la_info);
            dev.set_color(r, g, b, intensity)?;
        }
        DeviceInfo::LedRgb(rgb_info) => {
            let dev = LedRgbDevice::new(rgb_info);
            dev.set_color(r, g, b, intensity)?;
        }
    }
    let mut msg = format!("Set {} to ({}, {}, {})", info.name(), r, g, b);
    if intensity != 255 {
        msg.push_str(&format!(" intensity={intensity}"));
    }
    println!("{msg}");
    Ok(())
}

fn cmd_set_lamp(info: &DeviceInfo, lamps: &[(u16, String)], intensity: u8) -> Result<(), Error> {
    match info {
        DeviceInfo::LampArray(la_info) => {
            let dev = LampArrayDevice::new(la_info);
            let mut colors = Vec::with_capacity(lamps.len());
            for (id, color_str) in lamps {
                let rgb = match parse_color(std::slice::from_ref(color_str)) {
                    Some(c) => c,
                    None => {
                        return Err(Error::InvalidArgument(format!(
                            "Invalid color '{color_str}' for lamp {id}. \
                             Use a preset ({}), or a 6-digit hex code.",
                            preset_names()
                        )));
                    }
                };
                colors.push((*id, rgb.0, rgb.1, rgb.2, intensity));
            }
            dev.set_lamp_colors(&colors)?;
            println!("Set {} lamp(s) on {}", colors.len(), dev.name());
            Ok(())
        }
        DeviceInfo::LedRgb(_) => Err(Error::NoMultiUpdate),
    }
}

fn cmd_auto(info: &DeviceInfo, enabled: bool) -> Result<(), Error> {
    match info {
        DeviceInfo::LampArray(la_info) => {
            let dev = LampArrayDevice::new(la_info);
            dev.set_autonomous(enabled)?;
            let state = if enabled {
                "on (device controls)"
            } else {
                "off (host controls)"
            };
            println!("Autonomous mode: {state}");
            Ok(())
        }
        DeviceInfo::LedRgb(_) => Err(Error::NoAutonomousMode),
    }
}

// --- Help text ---

fn print_help() {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("hid-rgb-ctl {version} — Control RGB lighting on HID devices");
    eprintln!();
    eprintln!("Usage: hid-rgb-ctl [-p PATH] <command>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -V, --version    Show version and exit");
    eprintln!("  -p, --path PATH  hidraw device path (e.g. /dev/hidraw1)");
    eprintln!("                   If omitted, uses the first detected device.");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  list                     List detected RGB devices");
    eprintln!("  get                      Show device attributes and lamp info");
    eprintln!("  set COLOR [-i N]         Set all lamps to one color");
    eprintln!("  set-lamp ID:COLOR [...]  Set per-lamp colors (LampArray only)");
    eprintln!("  auto <on|off>            Toggle autonomous mode (LampArray only)");
    eprintln!();
    eprintln!("Color formats:");
    eprintln!("  Preset name:     red, green, blue, white, cyan, yellow,");
    eprintln!("                   orange, purple, pink, off");
    eprintln!("  Decimal RGB:     255 165 0");
    eprintln!("  Hex code:        ff6400 or #ff6400");
    eprintln!();
    eprintln!("set-lamp format:");
    eprintln!("  ID:COLOR pairs where ID is a lamp index (0-based) and COLOR");
    eprintln!("  is a preset name or hex code. Example: 0:red 1:00ff00 2:blue");
    eprintln!();
    eprintln!("Intensity:");
    eprintln!("  -i, --intensity N  Intensity 0-255 (default: 255)");
}

// --- Parsed CLI ---

enum Command {
    List,
    Get,
    Set {
        color: Vec<String>,
        intensity: u8,
    },
    SetLamp {
        lamps: Vec<(u16, String)>,
        intensity: u8,
    },
    Auto {
        state: String,
    },
}

struct Args {
    path: Option<String>,
    command: Option<Command>,
}

fn parse_set_lamp_args(
    parser: &mut lexopt::Parser,
) -> Result<(Vec<(u16, String)>, u8), lexopt::Error> {
    let mut lamps = Vec::new();
    let mut intensity = 255u8;

    while let Some(arg) = parser.next()? {
        match arg {
            Short('i') | Long("intensity") => {
                intensity = parser.value()?.parse()?;
            }
            Value(val) => {
                let s = val
                    .into_string()
                    .map_err(|_| "invalid UTF-8 in set-lamp argument")?;
                // Format: ID:COLOR (e.g. "0:red", "1:ff0000")
                let (id_str, color_str) = s
                    .split_once(':')
                    .ok_or("set-lamp arguments must be in ID:COLOR format (e.g. 0:red)")?;
                let id: u16 = id_str
                    .parse()
                    .map_err(|_| "lamp ID must be a non-negative integer")?;
                lamps.push((id, color_str.to_string()));
            }
            _ => return Err(arg.unexpected()),
        }
    }

    Ok((lamps, intensity))
}

fn parse_set_args(parser: &mut lexopt::Parser) -> Result<(Vec<String>, u8), lexopt::Error> {
    let mut color = Vec::new();
    let mut intensity = 255u8;

    while let Some(arg) = parser.next()? {
        match arg {
            Short('i') | Long("intensity") => {
                intensity = parser.value()?.parse()?;
            }
            Value(val) => {
                color.push(
                    val.into_string()
                        .map_err(|_| "invalid UTF-8 in color argument")?,
                );
            }
            _ => return Err(arg.unexpected()),
        }
    }

    Ok((color, intensity))
}

fn parse_args() -> Result<Args, Error> {
    let mut parser = lexopt::Parser::from_env();
    let mut path: Option<String> = None;
    let mut command: Option<Command> = None;

    while let Some(arg) = parser.next()? {
        match arg {
            Short('V') | Long("version") => {
                println!("hid-rgb-ctl {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            Short('h') | Long("help") => {
                print_help();
                std::process::exit(0);
            }
            Short('p') | Long("path") => {
                path =
                    Some(parser.value()?.into_string().map_err(|_| {
                        Error::InvalidArgument("invalid UTF-8 in path".to_string())
                    })?);
            }
            Value(val) if command.is_none() => {
                let s = val
                    .into_string()
                    .map_err(|_| Error::InvalidArgument("invalid UTF-8 in command".to_string()))?;
                match s.as_str() {
                    "list" => command = Some(Command::List),
                    "get" => command = Some(Command::Get),
                    "set" => {
                        let (color, intensity) = parse_set_args(&mut parser)?;
                        command = Some(Command::Set { color, intensity });
                    }
                    "set-lamp" => {
                        let (lamps, intensity) = parse_set_lamp_args(&mut parser)?;
                        command = Some(Command::SetLamp { lamps, intensity });
                    }
                    "auto" => {
                        let state_val = parser.value().map_err(|_| {
                            Error::InvalidArgument(
                                "auto command requires 'on' or 'off'".to_string(),
                            )
                        })?;
                        let state = state_val.into_string().map_err(|_| {
                            Error::InvalidArgument("invalid UTF-8 in auto state".to_string())
                        })?;
                        if state != "on" && state != "off" {
                            return Err(Error::InvalidArgument(format!(
                                "auto: expected 'on' or 'off', got '{state}'"
                            )));
                        }
                        command = Some(Command::Auto { state });
                    }
                    _ => {
                        return Err(Error::InvalidArgument(format!("unknown command: {s}")));
                    }
                }
            }
            _ => {
                return Err(arg.unexpected().into());
            }
        }
    }

    Ok(Args { path, command })
}

// --- Entry point ---

/// Run the CLI. Called from `main()`.
pub fn run() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {e}");
            print_help();
            std::process::exit(1);
        }
    };

    let command = match args.command {
        Some(cmd) => cmd,
        None => {
            print_help();
            std::process::exit(0);
        }
    };

    let devices = crate::descriptor::discover_devices();

    if let Command::List = &command {
        cmd_list(&devices);
        return;
    }

    // All other commands need a device
    let info = match find_device(&devices, args.path.as_deref()) {
        Some(d) => d,
        None => {
            if let Some(p) = &args.path {
                eprintln!("Error: No RGB device found at {p}");
            } else {
                eprintln!("Error: No HID RGB devices found.");
                eprintln!("Check permissions on /dev/hidraw* — see README for udev setup.");
            }
            std::process::exit(1);
        }
    };

    let result = match command {
        Command::List => unreachable!(),
        Command::Get => {
            cmd_get(info);
            Ok(())
        }
        Command::Set { color, intensity } => {
            let rgb = match parse_color(&color) {
                Some(c) => c,
                None => {
                    eprintln!(
                        "Error: Invalid color. Use a preset ({}), \
                         R G B values (0-255), or a 6-digit hex code.",
                        preset_names()
                    );
                    std::process::exit(1);
                }
            };
            cmd_set(info, rgb.0, rgb.1, rgb.2, intensity)
        }
        Command::SetLamp { lamps, intensity } => {
            if lamps.is_empty() {
                eprintln!("Error: set-lamp requires at least one ID:COLOR pair.");
                eprintln!("Example: hid-rgb-ctl set-lamp 0:red 1:00ff00 2:blue");
                std::process::exit(1);
            }
            cmd_set_lamp(info, &lamps, intensity)
        }
        Command::Auto { state } => cmd_auto(info, state == "on"),
    };

    if let Err(e) = result {
        match &e {
            Error::PermissionDenied { path } => {
                eprintln!("Error: Permission denied on {path}.");
                eprintln!("Run with sudo or set up a udev rule — see README.");
            }
            Error::Io(io_err) if io_err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("Error: Permission denied on {}.", info.hidraw_path());
                eprintln!("Run with sudo or set up a udev rule — see README.");
            }
            _ => {
                eprintln!("Error: {e}");
            }
        }
        std::process::exit(1);
    }
}
