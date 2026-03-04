//! Identifies and enumarates GPUs as PCI devices.

mod pci_ids;

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub enum Vendor {
    Amd,
    Intel,
    Nvidia,
}

impl Vendor {
    pub const fn name(&self) -> &'static str {
        match self {
            Vendor::Amd => "AMD",
            Vendor::Intel => "Intel",
            Vendor::Nvidia => "NVIDIA",
        }
    }

    pub fn get(symbol: &str) -> Option<Vendor> {
        match symbol {
            "amd" => Some(Self::Amd),
            "intel" => Some(Self::Intel),
            "nvidia" => Some(Self::Nvidia),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct PciDevice {
    pub vendor: Vendor,
    pub bus: u8,
    pub address: String,
    pub name: String,
}

#[cfg(target_os = "linux")]
fn parse_pci_addr(addr: &str) -> Option<(u16, u8, u8, u8)> {
    // PCI Address Format:
    // 0000:00:00.0 | <domain>:<bus>:<device>.<function>
    let mut parts = addr.split(|c| c == ':' || c == '.');
    let domain = u16::from_str_radix(parts.next()?, 16).ok()?;
    let bus = u8::from_str_radix(parts.next()?, 16).ok()?;
    let device = u8::from_str_radix(parts.next()?, 16).ok()?;
    let function = u8::from_str_radix(parts.next()?, 10).ok()?;
    Some((domain, bus, device, function))
}

#[cfg(target_os = "linux")]
fn parse_pci_id(id: &str) -> Option<(u16, u16)> {
    // PCI ID Format:
    // 0000:0000 | <vendor>:<device>
    let mut parts = id.split(':');
    let vendor = u16::from_str_radix(parts.next()?, 16).ok()?;
    let device = u16::from_str_radix(parts.next()?, 16).ok()?;
    Some((vendor, device))
}

/// Gets all GPUs from the PCI bus.
#[cfg(target_os = "linux")]
pub fn get_gpu_list() -> Vec<PciDevice> {
    use crate::error;
    use std::{fs::{read_dir, read_to_string}, process::exit};

    let pci_devices = read_dir("/sys/bus/pci/devices").unwrap_or_else(|_| {
        error!("Cannot read PCI devices");
        exit(1);
    });

    let mut gpus = Vec::new();
    let gpu_names = pci_ids::get_device_names();

    for device in pci_devices {
        let dir = device.unwrap();
        let uevent_file = dir.path().join("uevent");

        match read_to_string(uevent_file) {
            Ok(data) => {
                let mut driver = None;
                let mut pci_id = None;
                let mut subsys_id = None;
                for line in data.lines() {
                    if let Some(value) = line.strip_prefix("DRIVER=") {
                        driver = Some(value);
                    } else if let Some(value) = line.strip_prefix("PCI_ID=") {
                        pci_id = Some(value);
                    } else if let Some(value) = line.strip_prefix("PCI_SUBSYS_ID=") {
                        subsys_id = Some(value);
                    }
                }

                if let (Some(driver), Some(pci_id), Some(subsys_id)) = (driver, pci_id, subsys_id) {
                    let vendor = match driver {
                        "amdgpu" => Some(Vendor::Amd),
                        "nvidia" => Some(Vendor::Nvidia),
                        "xe" => Some(Vendor::Intel),
                        "i915" => {
                            // Check the first 2 digits of the device ID:
                            // 56xx: Arc A-Series
                            // E2xx: Arc B-Series
                            if ["56", "E2"].contains(&&pci_id[5..7]) { Some(Vendor::Intel) }
                            else { None }
                        }
                        _ => None,
                    };
                    if let Some(vendor) = vendor {
                        let pci_addr_str = dir.file_name().to_str().unwrap().to_owned();
                        let pci_addr = parse_pci_addr(&pci_addr_str).unwrap();
                        let pci_id = parse_pci_id(pci_id).unwrap();
                        let subsys_id = parse_pci_id(subsys_id).unwrap();
                        let gpu_name = if let Some(gpu_names) = &gpu_names {
                            // Look for subsystem ID (common on AMD devices)
                            if let Some(name) = gpu_names.get(&(vendor, pci_id.1, Some((subsys_id.0, subsys_id.1)))) { Some(name) }
                            // Fallback to device ID
                            else if let Some(name) = gpu_names.get(&(vendor, pci_id.1, None)) { Some(name) }
                            // Fallback to generic name
                            else { None }
                        } else { None };
                        // Unwrap the matched device name or specify generic name
                        let gpu_name = match gpu_name {
                            Some(name) => format!("{} {}", vendor.name(), name.to_owned()),
                            None => format!("{} {}", vendor.name(), if pci_addr.1 > 0 { "GPU" } else { "iGPU" })
                        };
                        gpus.push(
                            PciDevice {
                                vendor,
                                bus: pci_addr.1,
                                address: pci_addr_str,
                                name: gpu_name
                            }
                        );
                    }
                }
            }
            Err(_) => (),
        }
    }

    gpus
}

/// Gets all GPUs using DXGI on Windows.
#[cfg(target_os = "windows")]
pub fn get_gpu_list() -> Vec<PciDevice> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_DESC1};

    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let mut gpus = Vec::new();
    let mut idx = 0u32;

    loop {
        let adapter = match unsafe { factory.EnumAdapters1(idx) } {
            Ok(a) => a,
            Err(_) => break,
        };
        idx += 1;

        let desc: DXGI_ADAPTER_DESC1 = match unsafe { adapter.GetDesc1() } {
            Ok(d) => d,
            Err(_) => continue,
        };

        let vendor = match desc.VendorId {
            0x1002 => Vendor::Amd,
            0x10DE => Vendor::Nvidia,
            0x8086 => Vendor::Intel,
            _ => continue,
        };

        // Convert wide string description to String
        let name_len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..name_len]);

        // Use DXGI adapter index as a synthetic address
        let address = format!("dxgi:{}", idx - 1);
        // Treat adapter index 0 with Intel as iGPU (bus=0), others as discrete (bus=1)
        let bus = if vendor == Vendor::Intel && idx == 1 { 0 } else { 1 };

        gpus.push(PciDevice {
            vendor,
            bus,
            address,
            name: format!("{} {}", vendor.name(), name),
        });
    }

    gpus
}
