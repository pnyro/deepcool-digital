//! WinRing0 kernel driver interface for reading PCI config space and SMN registers.
//! Used to read AMD Ryzen CPU temperature on Windows.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, HANDLE, GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Services::{
    CloseServiceHandle, CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, StartServiceW,
    SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL,
    SERVICE_KERNEL_DRIVER,
};
use windows::core::PCWSTR;

const DEVICE_NAME: &str = "\\\\.\\WinRing0_1_2_0";
const SERVICE_NAME: &str = "WinRing0_1_2_0";
const DRIVER_FILE: &str = "WinRing0x64.sys";

// IOCTL codes: CTL_CODE(40000, function, METHOD_BUFFERED, access)
// CTL_CODE = (device_type << 16) | (access << 14) | (function << 2) | method
const IOCTL_READ_PCI_CONFIG: u32 = (40000 << 16) | (1 << 14) | (0x851 << 2);
const IOCTL_WRITE_PCI_CONFIG: u32 = (40000 << 16) | (2 << 14) | (0x852 << 2);

// AMD Ryzen SMN temperature register
const SMN_THM_TCON_CUR_TMP: u32 = 0x00059800;
// PCI config offsets for SMN access (host bridge: bus 0, device 0, function 0)
const SMN_ADDR_REG: u32 = 0x60;
const SMN_DATA_REG: u32 = 0x64;

pub struct WinRing0 {
    device: HANDLE,
}

impl WinRing0 {
    /// Loads the WinRing0 driver and opens the device.
    /// The .sys file must be next to the executable.
    pub fn new() -> Option<Self> {
        let driver_path = find_driver()?;
        install_driver(&driver_path)?;
        let device = open_device()?;
        Some(WinRing0 { device })
    }

    /// Reads the AMD Ryzen CPU temperature in °C.
    pub fn read_cpu_temp(&self) -> Option<f32> {
        // Write SMN address to PCI config register 0x60
        self.write_pci_config(0, SMN_ADDR_REG, SMN_THM_TCON_CUR_TMP)?;
        // Read result from PCI config register 0x64
        let value = self.read_pci_config(0, SMN_DATA_REG)?;
        // Bits [31:21] contain temperature in 0.125°C units
        let raw_temp = (value >> 21) & 0x7FF;
        let mut temp = raw_temp as f32 * 0.125;
        // Bit 19: Tctl offset flag (AMD Ryzen reports Tctl, not Tdie)
        if (value >> 19) & 1 == 1 {
            temp -= 49.0;
        }
        Some(temp)
    }

    fn read_pci_config(&self, pci_address: u32, offset: u32) -> Option<u32> {
        let input = [pci_address, offset];
        let mut output: u32 = 0;
        let mut bytes_returned: u32 = 0;

        let ok = unsafe {
            DeviceIoControl(
                self.device,
                IOCTL_READ_PCI_CONFIG,
                Some(input.as_ptr() as *const _),
                8,
                Some(&mut output as *mut _ as *mut _),
                4,
                Some(&mut bytes_returned),
                None,
            )
        };

        if ok.is_ok() && bytes_returned == 4 {
            Some(output)
        } else {
            None
        }
    }

    fn write_pci_config(&self, pci_address: u32, offset: u32, value: u32) -> Option<()> {
        let input = [pci_address, offset, value];
        let mut bytes_returned: u32 = 0;

        let ok = unsafe {
            DeviceIoControl(
                self.device,
                IOCTL_WRITE_PCI_CONFIG,
                Some(input.as_ptr() as *const _),
                12,
                None,
                0,
                Some(&mut bytes_returned),
                None,
            )
        };

        if ok.is_ok() { Some(()) } else { None }
    }
}

impl Drop for WinRing0 {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.device); }
        uninstall_driver();
    }
}

/// Finds the WinRing0x64.sys driver file next to the executable.
fn find_driver() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let path = dir.join(DRIVER_FILE);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Converts a Rust string to a null-terminated wide string.
fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Installs and starts the WinRing0 kernel driver as a temporary service.
fn install_driver(driver_path: &PathBuf) -> Option<()> {
    let service_name = to_wide(SERVICE_NAME);
    let driver_path_str = driver_path.to_str()?;
    let driver_path_wide = to_wide(driver_path_str);

    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS).ok()?;

        // Try to open existing service first
        let service = OpenServiceW(scm, PCWSTR(service_name.as_ptr()), SERVICE_ALL_ACCESS);

        let service = if let Ok(svc) = service {
            svc
        } else {
            // Create new service
            CreateServiceW(
                scm,
                PCWSTR(service_name.as_ptr()),
                PCWSTR(service_name.as_ptr()),
                SERVICE_ALL_ACCESS,
                SERVICE_KERNEL_DRIVER,
                SERVICE_DEMAND_START,
                SERVICE_ERROR_NORMAL,
                PCWSTR(driver_path_wide.as_ptr()),
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
            ).ok()?
        };

        // Start the service (ignore error if already running)
        let _ = StartServiceW(service, None);

        CloseServiceHandle(service).ok()?;
        CloseServiceHandle(scm).ok()?;
    }

    Some(())
}

/// Opens the WinRing0 device handle.
fn open_device() -> Option<HANDLE> {
    let device_name = to_wide(DEVICE_NAME);

    let handle = unsafe {
        CreateFileW(
            PCWSTR(device_name.as_ptr()),
            (GENERIC_READ.0 | GENERIC_WRITE.0).into(),
            windows::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ).ok()?
    };

    Some(handle)
}

/// Stops and removes the WinRing0 service.
fn uninstall_driver() {
    let service_name = to_wide(SERVICE_NAME);

    unsafe {
        if let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) {
            if let Ok(service) = OpenServiceW(scm, PCWSTR(service_name.as_ptr()), SERVICE_ALL_ACCESS) {
                use windows::Win32::System::Services::{ControlService, SERVICE_CONTROL_STOP, SERVICE_STATUS};
                let mut status = SERVICE_STATUS::default();
                let _ = ControlService(service, SERVICE_CONTROL_STOP, &mut status);
                let _ = DeleteService(service);
                let _ = CloseServiceHandle(service);
            }
            let _ = CloseServiceHandle(scm);
        }
    }
}
