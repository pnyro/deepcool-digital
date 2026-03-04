//! Reads live GPU data from AMD GPUs.

#[cfg(target_os = "linux")]
mod platform {
    use crate::error;
    use std::{fs::{read_dir, read_to_string}, process::exit};

    pub struct Gpu {
        usage_file: String,
        hwmon_dir: String,
    }

    impl Gpu {
        pub fn new(pci_address: &str) -> Self {
            let path = format!("/sys/bus/pci/devices/{pci_address}");

            let usage_file = match find_card(&path) {
                Some(file) => file,
                None => {
                    error!(format!("Failed access GPU (AMD) PCI_ADDR={pci_address}"));
                    exit(1);
                }
            };

            let hwmon_dir = match find_hwmon_dir(&path) {
                Some(dir) => dir,
                None => {
                    error!("Failed to locate GPU temperature sensor (AMD)");
                    exit(1);
                }
            };

            Gpu { usage_file, hwmon_dir }
        }

        /// Reads the value of the GPU temperature sensor and calculates it to be `˚C` or `˚F`.
        pub fn get_temp(&self, fahrenheit: bool) -> u8 {
            // Read sensor data
            let data = read_to_string(format!("{}/temp1_input", &self.hwmon_dir)).unwrap_or_else(|_| {
                error!("Failed to get GPU temperature (AMD)");
                exit(1);
            });

            // Calculate temperature
            let mut temp = data.trim_end().parse::<u32>().unwrap();
            if fahrenheit {
                temp = temp * 9 / 5 + 32000
            }

            (temp as f32 / 1000.0).round() as u8
        }

        /// Reads the value of the GPU usage in percentage.
        pub fn get_usage(&self) -> u8 {
            let data = read_to_string(&self.usage_file).unwrap_or_else(|_| {
                error!("Failed to get GPU usage (AMD)");
                exit(1);
            });

            data.trim_end().parse::<u8>().unwrap()
        }

        /// Reads the value of the GPU power consumption in Watts.
        pub fn get_power(&self) -> u16 {
            let data = read_to_string(format!("{}/power1_average", &self.hwmon_dir)).unwrap_or_else(|_| {
                error!("Failed to get GPU power (AMD)");
                exit(1);
            });
            let power = data.trim_end().parse::<u64>().unwrap();

            (power / 1_000_000) as u16
        }

        /// Reads the value of the GPU core frequency in MHz.
        pub fn get_frequency(&self) -> u16 {
            let data = read_to_string(format!("{}/freq1_input", &self.hwmon_dir)).unwrap_or_else(|_| {
                error!("Failed to get GPU core frequency (AMD)");
                exit(1);
            });
            let frequency = data.trim_end().parse::<u64>().unwrap();

            (frequency / 1_000_000) as u16
        }
    }

    /// Confirms that the specified path belongs to an AMD GPU and returns the path of the "GPU Usage" file.
    fn find_card(path: &str) -> Option<String> {
        if let Ok(data) = read_to_string(format!("{path}/uevent")) {
            let driver = data.lines().next()?;
            if driver.ends_with("amdgpu") {
                return Some(format!("{path}/gpu_busy_percent"));
            }
        }

        None
    }

    /// Looks for the hwmon directory of the specified AMD GPU.
    fn find_hwmon_dir(path: &str) -> Option<String> {
        let hwmon_path = read_dir(format!("{path}/hwmon")).ok()?.next()?.ok()?.path();
        if let Ok(name) = read_to_string(hwmon_path.join("name")) {
            if name.starts_with("amdgpu") {
                return Some(hwmon_path.to_str()?.to_owned());
            }
        }

        None
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use crate::error;
    use libloading::{Library, Symbol};
    use std::{process::exit, ptr::null_mut};

    // ADL return codes
    const ADL_OK: i32 = 0;

    // ADL malloc callback — ADL requires the caller to provide memory allocation
    unsafe extern "C" fn adl_malloc(size: i32) -> *mut std::ffi::c_void {
        std::alloc::alloc(std::alloc::Layout::from_size_align_unchecked(size as usize, 1)) as *mut std::ffi::c_void
    }

    // ADL function types
    type AdlMainControlCreate = unsafe extern "C" fn(malloc_callback: unsafe extern "C" fn(i32) -> *mut std::ffi::c_void, enumerate: i32) -> i32;
    type AdlMainControlDestroy = unsafe extern "C" fn() -> i32;
    type Adl2AdapterNumberOfAdaptersGet = unsafe extern "C" fn(context: *mut std::ffi::c_void, num: *mut i32) -> i32;
    type Adl2AdapterAdapterInfoGet = unsafe extern "C" fn(context: *mut std::ffi::c_void, info: *mut AdapterInfo, size: i32) -> i32;

    // Performance monitoring via ADL_Overdrive
    type Adl2OverdriveNTemperatureGet = unsafe extern "C" fn(context: *mut std::ffi::c_void, adapter: i32, sensor: i32, temp: *mut i32) -> i32;
    type Adl2OverdriveNPerformanceStatusGet = unsafe extern "C" fn(context: *mut std::ffi::c_void, adapter: i32, status: *mut ADLODNPerformanceStatus) -> i32;

    #[repr(C)]
    struct AdapterInfo {
        size: i32,
        adapter_index: i32,
        udid: [u8; 256],
        bus_number: i32,
        device_number: i32,
        function_number: i32,
        vendor_id: i32,
        adapter_name: [u8; 256],
        display_name: [u8; 256],
        present: i32,
        exist: i32,
        driver_path: [u8; 256],
        driver_path_ext: [u8; 256],
        pnp_string: [u8; 256],
        os_display_index: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ADLODNPerformanceStatus {
        core_clock: i32,
        memory_clock: i32,
        dc_ef_clock: i32,
        gfx_clock: i32,
        override_gfx_clock: i32,
        override_mem_clock: i32,
        vddc: i32,
        vddci: i32,
        current_bus_speed: i32,
        current_bus_lanes: i32,
        maximum_bus_lanes: i32,
        activity: i32,
        current_core_performance_level: i32,
        current_memory_performance_level: i32,
        current_dc_ef_performance_level: i32,
        current_gfx_performance_level: i32,
    }

    pub struct Gpu {
        lib: Library,
        adapter_index: i32,
    }

    impl Gpu {
        pub fn new(_pci_address: &str) -> Self {
            unsafe {
                let lib = Library::new("atiadlxx.dll")
                    .or_else(|_| Library::new("atiadlxy.dll"))
                    .unwrap_or_else(|_| {
                        error!("AMD GPU library (atiadlxx.dll) was not found");
                        exit(1);
                    });

                // Initialize ADL with our malloc callback
                let adl_init: Symbol<AdlMainControlCreate> = lib.get(b"ADL_Main_Control_Create").unwrap();
                if adl_init(adl_malloc, 1) != ADL_OK {
                    error!("Failed to initialize AMD ADL");
                    exit(1);
                }

                // Get number of adapters
                let mut num_adapters: i32 = 0;
                let get_num: Symbol<Adl2AdapterNumberOfAdaptersGet> = lib.get(b"ADL_Adapter_NumberOfAdapters_Get").unwrap();
                get_num(null_mut(), &mut num_adapters);

                // Find first active adapter
                let mut adapter_index = 0;
                if num_adapters > 0 {
                    let count = num_adapters as usize;
                    let size = (count * std::mem::size_of::<AdapterInfo>()) as i32;
                    let mut adapters: Vec<AdapterInfo> = Vec::with_capacity(count);
                    // Safety: AdapterInfo is a plain C struct, zero-init is valid
                    std::ptr::write_bytes(adapters.as_mut_ptr(), 0, count);
                    adapters.set_len(count);
                    let get_info: Symbol<Adl2AdapterAdapterInfoGet> = lib.get(b"ADL_Adapter_AdapterInfo_Get").unwrap();
                    get_info(null_mut(), adapters.as_mut_ptr(), size);

                    // Use the first adapter (index 0) or find the one matching our DXGI address
                    for adapter in &adapters {
                        if adapter.vendor_id == 0x1002 && adapter.present != 0 {
                            adapter_index = adapter.adapter_index;
                            break;
                        }
                    }
                }

                Gpu { lib, adapter_index }
            }
        }

        pub fn get_temp(&self, fahrenheit: bool) -> u8 {
            let mut temp: i32 = 0;
            unsafe {
                if let Ok(get_temp) = self.lib.get::<Adl2OverdriveNTemperatureGet>(b"ADL2_OverdriveN_Temperature_Get") {
                    // sensor type 1 = GPU edge temperature
                    get_temp(null_mut(), self.adapter_index, 1, &mut temp);
                }
            }

            let temp_c = (temp as f32 / 1000.0).round();
            if fahrenheit {
                (temp_c * 9.0 / 5.0 + 32.0).round() as u8
            } else {
                temp_c as u8
            }
        }

        pub fn get_usage(&self) -> u8 {
            let status = self.get_perf_status();
            status.activity.clamp(0, 100) as u8
        }

        pub fn get_power(&self) -> u16 {
            // ADL OverdriveN doesn't directly expose power in a simple way.
            // Return 0 for now — power monitoring requires ADL2_Overdrive8_Current_Setting_Get
            // which is only available on newer RDNA cards.
            0
        }

        pub fn get_frequency(&self) -> u16 {
            let status = self.get_perf_status();
            // core_clock is in MHz * 100 on some cards, or plain MHz on others
            let clock = status.core_clock;
            if clock > 10000 {
                (clock / 100) as u16
            } else {
                clock as u16
            }
        }

        fn get_perf_status(&self) -> ADLODNPerformanceStatus {
            let mut status = ADLODNPerformanceStatus::default();
            unsafe {
                if let Ok(get_status) = self.lib.get::<Adl2OverdriveNPerformanceStatusGet>(b"ADL2_OverdriveN_PerformanceStatus_Get") {
                    get_status(null_mut(), self.adapter_index, &mut status);
                }
            }
            status
        }
    }

    impl Drop for Gpu {
        fn drop(&mut self) {
            unsafe {
                if let Ok(destroy) = self.lib.get::<AdlMainControlDestroy>(b"ADL_Main_Control_Destroy") {
                    destroy();
                }
            }
        }
    }
}

pub use platform::Gpu;
