//! Reads live CPU data.

#[cfg(target_os = "linux")]
mod platform {
    use crate::{error, warning};
    use cpu_monitor::CpuInstant;
    use std::{fs::{read_dir, read_to_string, File}, io::{BufRead, BufReader}, process::exit};

    pub struct Cpu {
        temp_sensor: Option<String>,
        rapl_max_uj: u64,
    }

    impl Cpu {
        pub fn new() -> Self {
            Cpu {
                temp_sensor: find_temp_sensor(),
                rapl_max_uj: get_max_energy(),
            }
        }

        /// Displays a warning message if temperature sensor is not initialized.
        pub fn warn_temp(&self) {
            if self.temp_sensor == None {
                warning!("No supported CPU temperature sensor was found");
                eprintln!("         CPU temperature will not be displayed, and alarm will be disabled.");
                eprintln!("         Supported kernel modules are: asusec, coretemp, k10temp, and zenpower.");
            }
        }

        /// Displays a warning message if RAPL module is not initialized.
        pub fn warn_rapl(&self) {
            if self.rapl_max_uj == 0 {
                warning!("RAPL module was not found");
                eprintln!("         CPU power consumption will not be displayed.");
            }
        }

        /// Reads the value of the CPU temperature sensor and calculates it to be `˚C` or `˚F`.
        pub fn get_temp(&self, fahrenheit: bool) -> u8 {
            if let Some(sensor) = &self.temp_sensor {
                // Read sensor data
                let data = read_to_string(sensor).unwrap_or_else(|_| {
                    error!("Failed to get CPU temperature");
                    exit(1);
                });
                // Calculate temperature
                let mut temp = data.trim_end().parse::<u32>().unwrap();
                if fahrenheit {
                    temp = temp * 9 / 5 + 32000
                }
                return (temp as f32 / 1000.0).round() as u8;
            }

            0
        }

        /// Reads the energy consumption of the CPU in microjoules.
        pub fn read_energy(&self) -> u64 {
            if self.rapl_max_uj > 0 {
                let data = read_to_string("/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj").unwrap_or_else(|_| {
                    error!("Failed to get CPU power");
                    exit(1);
                });
                return data.trim_end().parse::<u64>().unwrap();
            }

            0
        }

        /// Reads the energy consumption one more time and calculates the CPU power by using the inital energy and the delta time.
        ///
        /// Formula: `W = ΔμJ / (Δms * 1000)`
        pub fn get_power(&self, initial_energy: u64, delta_millisec: u64) -> u16 {
            if self.rapl_max_uj > 0 {
                let current_energy = self.read_energy();
                let delta_energy = if current_energy > initial_energy {
                    current_energy - initial_energy
                } else {
                    // Offset the current measurement if the counter resets
                    (self.rapl_max_uj + current_energy) - initial_energy
                };
                return (delta_energy as f64 / (delta_millisec * 1000) as f64).round() as u16;
            }

            0
        }

        /// Reads the CPU instant and provides usage statistics.
        pub fn read_instant(&self) -> CpuInstant {
            CpuInstant::now().unwrap_or_else(|_| {
                error!("Failed to get CPU usage");
                exit(1);
            })
        }

        /// Reads the CPU instant one more time and calculates the utilization as a `0-100` number.
        pub fn get_usage(&self, initial_instant: CpuInstant) -> u8 {
            let usage = (self.read_instant() - initial_instant).non_idle() * 100.0;

            (usage).round() as u8
        }

        /// Reads the frequency of all CPU cores and returns the highest one in MHz.
        pub fn get_frequency(&self) -> u16 {
            let cpuinfo = read_to_string("/proc/cpuinfo").unwrap_or_else(|_| {
                error!("Failed to get CPU clock");
                exit(1);
            });

            let mut highest_core = 0.0;
            for info in cpuinfo.lines() {
                if info.starts_with("cpu MHz") {
                    let clock = info.split(":").nth(1).unwrap();
                    let clock = clock.trim().parse::<f32>().unwrap();
                    if clock > highest_core {
                        highest_core = clock;
                    }
                }
            }

            highest_core.round() as u16
        }
    }

    /// Looks for the appropriate CPU temperature sensor datastream in the hwmon directory.
    fn find_temp_sensor() -> Option<String> {
        for sensor in read_dir("/sys/class/hwmon").ok()? {
            let path = sensor.ok()?.path().to_str()?.to_owned();
            if let Ok(name) = read_to_string(format!("{path}/name")) {
                if ["asusec", "coretemp", "k10temp", "zenpower"].contains(&name.trim_end()) {
                    return Some(format!("{path}/temp1_input"));
                }
            }
        }

        None
    }

    /// Gets the limit of the displayed energy value so it can be applied as an offset when the counter resets.
    fn get_max_energy() -> u64 {
        match read_to_string("/sys/class/powercap/intel-rapl/intel-rapl:0/max_energy_range_uj") {
            Ok(data) => data.trim_end().parse::<u64>().unwrap(),
            Err(_) => 0,
        }
    }

    /// Gets the CPU model name.
    pub fn get_name() -> Option<String> {
        let file = File::open("/proc/cpuinfo").ok()?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.ok()?;
            if line.starts_with("model name") {
                if let Some(colon_pos) = line.find(':') {
                    return Some(line[colon_pos + 1..].trim().to_string());
                }
            }
        }

        None
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use crate::{error, warning};
    use cpu_monitor::CpuInstant;
    use std::collections::HashMap;
    use std::process::exit;
    use wmi::{COMLibrary, Variant, WMIConnection};

    enum TempSource {
        /// Direct SMN register access via WinRing0 driver (AMD Ryzen)
        WinRing0(crate::monitor::winring0::WinRing0),
        /// MSAcpi_ThermalZoneTemperature (root\WMI) — returns Kelvin × 10
        Acpi(WMIConnection),
        /// LibreHardwareMonitor WMI sensor (root\LibreHardwareMonitor)
        Lhm(WMIConnection),
        /// No temperature source available
        None,
    }

    pub struct Cpu {
        temp_source: TempSource,
        wmi_root: Option<WMIConnection>,
    }

    fn init_com() -> Option<COMLibrary> {
        COMLibrary::new()
            .or_else(|_| COMLibrary::without_security())
            .ok()
    }

    /// Extract a u32 from a WMI Variant
    fn variant_u32(v: &Variant) -> Option<u32> {
        match v {
            Variant::UI4(n) => Some(*n),
            Variant::I4(n) => Some(*n as u32),
            Variant::UI8(n) => Some(*n as u32),
            Variant::I8(n) => Some(*n as u32),
            _ => None,
        }
    }

    /// Extract a f32 from a WMI Variant
    fn variant_f32(v: &Variant) -> Option<f32> {
        match v {
            Variant::R4(n) => Some(*n),
            Variant::R8(n) => Some(*n as f32),
            Variant::UI4(n) => Some(*n as f32),
            Variant::I4(n) => Some(*n as f32),
            _ => None,
        }
    }

    /// Extract a String from a WMI Variant
    fn variant_string(v: &Variant) -> Option<String> {
        match v {
            Variant::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    impl Cpu {
        pub fn new() -> Self {
            let com = match init_com() {
                Some(c) => c,
                None => return Cpu { temp_source: TempSource::None, wmi_root: None },
            };

            let wmi_root = WMIConnection::new(com).ok();

            // Try WinRing0 driver first (direct hardware access, works on AMD Ryzen)
            let ring0: Option<crate::monitor::winring0::WinRing0> = crate::monitor::winring0::WinRing0::new();
            if let Some(ring0) = ring0 {
                if ring0.read_cpu_temp().is_some() {
                    return Cpu { temp_source: TempSource::WinRing0(ring0), wmi_root };
                }
            }

            // Fallback: ACPI thermal zone (works on some Intel boards)
            let temp_source = if let Some(com2) = init_com() {
                if let Ok(conn) = WMIConnection::with_namespace_path("root\\WMI", com2) {
                    let works: bool = conn.raw_query(
                        "SELECT CurrentTemperature FROM MSAcpi_ThermalZoneTemperature"
                    ).map_or(false, |r: Vec<HashMap<String, Variant>>| !r.is_empty());
                    if works {
                        TempSource::Acpi(conn)
                    } else {
                        try_lhm()
                    }
                } else {
                    try_lhm()
                }
            } else {
                TempSource::None
            };

            Cpu { temp_source, wmi_root }
        }

        pub fn warn_temp(&self) {
            if matches!(self.temp_source, TempSource::None) {
                warning!("No CPU temperature sensor was found");
                eprintln!("         CPU temperature will not be displayed, and alarm will be disabled.");
                eprintln!("         Ensure WinRing0x64.sys is in the same directory as the executable, and run as Administrator.");
            }
        }

        pub fn warn_rapl(&self) {
            warning!("CPU power monitoring is not available on Windows");
            eprintln!("         CPU power consumption will not be displayed.");
        }

        pub fn get_temp(&self, fahrenheit: bool) -> u8 {
            let celsius = match &self.temp_source {
                TempSource::WinRing0(ring0) => {
                    ring0.read_cpu_temp().unwrap_or(0.0)
                }
                TempSource::Acpi(conn) => {
                    let results: Vec<HashMap<String, Variant>> = conn.raw_query(
                        "SELECT CurrentTemperature FROM MSAcpi_ThermalZoneTemperature"
                    ).unwrap_or_default();
                    results.first()
                        .and_then(|r| r.get("CurrentTemperature"))
                        .and_then(variant_u32)
                        .map(|kt| (kt as f32 / 10.0) - 273.15)
                        .unwrap_or(0.0)
                }
                TempSource::Lhm(conn) => {
                    // Try "CPU Package" first, then AMD Ryzen "Core (Tctl/Tdie)"
                    let results: Vec<HashMap<String, Variant>> = conn.raw_query(
                        "SELECT Value, Name FROM Sensor WHERE SensorType='Temperature'"
                    ).unwrap_or_default();

                    results.iter()
                        .find(|r| {
                            r.get("Name").and_then(variant_string)
                                .map_or(false, |n| n == "CPU Package" || n.contains("Tctl"))
                        })
                        .and_then(|r| r.get("Value"))
                        .and_then(variant_f32)
                        .unwrap_or(0.0)
                }
                TempSource::None => return 0,
            };

            if fahrenheit {
                (celsius * 9.0 / 5.0 + 32.0).round() as u8
            } else {
                celsius.round() as u8
            }
        }

        pub fn read_energy(&self) -> u64 {
            0
        }

        pub fn get_power(&self, _initial_energy: u64, _delta_millisec: u64) -> u16 {
            0
        }

        pub fn read_instant(&self) -> CpuInstant {
            CpuInstant::now().unwrap_or_else(|_| {
                error!("Failed to get CPU usage");
                exit(1);
            })
        }

        pub fn get_usage(&self, initial_instant: CpuInstant) -> u8 {
            let usage = (self.read_instant() - initial_instant).non_idle() * 100.0;
            (usage).round() as u8
        }

        pub fn get_frequency(&self) -> u16 {
            if let Some(conn) = &self.wmi_root {
                let results: Vec<HashMap<String, Variant>> = conn.raw_query(
                    "SELECT CurrentClockSpeed FROM Win32_Processor"
                ).unwrap_or_default();
                if let Some(first) = results.first() {
                    if let Some(speed) = first.get("CurrentClockSpeed").and_then(variant_u32) {
                        return speed as u16;
                    }
                }
            }
            0
        }
    }

    /// Try to connect to LibreHardwareMonitor's WMI namespace
    fn try_lhm() -> TempSource {
        for namespace in ["root\\LibreHardwareMonitor", "root\\OpenHardwareMonitor"] {
            if let Some(com) = init_com() {
                if let Ok(conn) = WMIConnection::with_namespace_path(namespace, com) {
                    let works: bool = conn.raw_query(
                        "SELECT Value FROM Sensor WHERE SensorType='Temperature'"
                    ).map_or(false, |r: Vec<HashMap<String, Variant>>| !r.is_empty());
                    if works {
                        return TempSource::Lhm(conn);
                    }
                }
            }
        }
        TempSource::None
    }

    pub fn get_name() -> Option<String> {
        let com = init_com()?;
        let conn = WMIConnection::new(com).ok()?;
        let results: Vec<HashMap<String, Variant>> = conn.raw_query(
            "SELECT Name FROM Win32_Processor"
        ).ok()?;
        results.first()
            .and_then(|r| r.get("Name"))
            .and_then(variant_string)
            .map(|s| s.trim().to_string())
    }
}

pub use platform::*;
