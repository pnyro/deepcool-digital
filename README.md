# DeepCool Digital (Windows)

A lightweight, open-source alternative to DeepCool's official Windows software.

This is a Windows fork of [deepcool-digital-linux](https://github.com/Nortank12/deepcool-digital-linux).
It drives the small status display on DeepCool coolers and cases, showing CPU/GPU temperature, usage,
power, and clock speed. No telemetry, no background services, no unnecessary UI — just a single
executable.

> **Linux users**: Head to the [original project](https://github.com/Nortank12/deepcool-digital-linux).

# Installation
1. Download `deepcool-digital-windows-amd64.exe` and `WinRing0x64.sys` from the latest [release](https://github.com/pnyro/deepcool-digital/releases)
2. Place both files in the same folder
3. Run the exe as **Administrator** (required for USB HID access and CPU temperature)

> [!NOTE]
> `WinRing0x64.sys` is a signed kernel driver used to read CPU temperature directly from AMD Ryzen
> processors. Without it, CPU temperature will not be available (usage and other stats still work).
> Windows Defender may flag it — this is a [known false positive](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/issues/1660)
> common to all hardware monitoring tools. You can verify the driver's digital signature:
> ```powershell
> Get-AuthenticodeSignature .\WinRing0x64.sys
> ```

# Supported Devices

All devices supported by the [original project](https://github.com/Nortank12/deepcool-digital-linux#supported-devices) are supported on Windows.

# Usage
```
deepcool-digital-windows-amd64.exe [OPTIONS]
```
```
Options:
  -m, --mode <MODE>       Change the display mode of your device
  -s, --secondary <MODE>  Change the secondary display mode of your device (if supported)
      --pid <ID>          Specify the Product ID if multiple devices are connected
      --gpuid <VENDOR:ID> Specify the nth GPU of a specific vendor to monitor (use ID 0 for integrated GPU)

  -u, --update <MILLISEC> Change the update interval of the display [default: 1000]
  -f, --fahrenheit        Change the temperature unit to °F
  -a, --alarm             Enable the alarm

Commands:
  -l, --list         Print Product ID of the connected devices
  -g, --gpulist      Print all available GPUs
  -h, --help         Print help
  -v, --version      Print version
```

# Automatic Start
Create a scheduled task that runs at logon with admin privileges:
```powershell
schtasks /create /tn "DeepCool Digital" /tr "C:\path\to\deepcool-digital-windows-amd64.exe" /sc onlogon /rl highest
```

# Building from Source
1. Install [Rust](https://rustup.rs/)
2. Clone and build:
```powershell
git clone https://github.com/pnyro/deepcool-digital
cd deepcool-digital
cargo build --release
```
The binary will be in `target\release\`.

# Windows Implementation Notes
- **CPU temperature**: Read directly from AMD Ryzen SMN registers via WinRing0 kernel driver. Falls back to ACPI thermal zone (Intel) or returns 0 if unavailable.
- **CPU usage/frequency**: Cross-platform `cpu-monitor` crate and WMI `Win32_Processor`.
- **GPU (NVIDIA)**: NVML via `nvml.dll` (requires NVIDIA drivers).
- **GPU (AMD)**: AMD Display Library (ADL) via `atiadlxx.dll` (requires AMD drivers).
- **GPU (Intel)**: Not yet supported on Windows.
- **CPU power**: Not available on Windows (no RAPL equivalent).

All Windows code is behind `#[cfg(target_os = "windows")]` — the original Linux code is untouched.
