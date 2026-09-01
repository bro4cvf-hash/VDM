use std::process::Command;

const APP_NAME: &str = "VDM";
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Check if VDM is configured to run at Windows startup and not disabled in Task Manager / Startup Apps.
pub fn is_startup_enabled() -> bool {
    #[cfg(windows)]
    {
        is_startup_enabled_win()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Enable or disable VDM at Windows startup, synchronizing with Task Manager / Startup Apps.
pub fn set_startup_enabled(enabled: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        set_startup_enabled_win(enabled)
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(())
    }
}

/// Launch Windows Settings -> Apps -> Startup or Task Manager to let the user inspect startup apps.
pub fn open_windows_startup_settings() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Try opening Windows 10/11 Settings -> Apps -> Startup first
        let _ = Command::new("cmd")
            .creation_flags(0x08000000)
            .args(["/C", "start", "ms-settings:startupapps"])
            .spawn();
    }
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn is_startup_enabled_win() -> bool {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_BINARY, REG_SZ,
    };

    unsafe {
        // 1. Check if the value exists in HKCU\...\Run
        let run_sub = to_wide(RUN_KEY_PATH);
        let val_name = to_wide(APP_NAME);
        let mut h_run: HKEY = null_mut();

        if RegOpenKeyExW(HKEY_CURRENT_USER, run_sub.as_ptr(), 0, KEY_READ, &mut h_run) != ERROR_SUCCESS {
            return false;
        }

        let mut val_type: u32 = 0;
        let mut data_size: u32 = 0;
        let status = RegQueryValueExW(h_run, val_name.as_ptr(), null_mut(), &mut val_type, null_mut(), &mut data_size);
        RegCloseKey(h_run);

        if status != ERROR_SUCCESS || (val_type != REG_SZ && val_type != windows_sys::Win32::System::Registry::REG_EXPAND_SZ) {
            return false;
        }

        // 2. Check if Task Manager / Startup Apps disabled it in HKCU\...\StartupApproved\Run
        let apprv_sub = to_wide(STARTUP_APPROVED_KEY_PATH);
        let mut h_apprv: HKEY = null_mut();

        if RegOpenKeyExW(HKEY_CURRENT_USER, apprv_sub.as_ptr(), 0, KEY_READ, &mut h_apprv) == ERROR_SUCCESS {
            let mut bin_type: u32 = 0;
            let mut bin_buf = [0u8; 32];
            let mut bin_size: u32 = bin_buf.len() as u32;

            let apprv_status = RegQueryValueExW(
                h_apprv,
                val_name.as_ptr(),
                null_mut(),
                &mut bin_type,
                bin_buf.as_mut_ptr(),
                &mut bin_size,
            );
            RegCloseKey(h_apprv);

            if apprv_status == ERROR_SUCCESS && bin_type == REG_BINARY && bin_size > 0 {
                // Windows convention: If the first byte's lowest bit is 1 (e.g. 0x01, 0x03), it is Disabled by Task Manager.
                // If it is 0x02, 0x00, 0x06 (lowest bit 0), it is Enabled.
                if (bin_buf[0] & 1) != 0 {
                    return false;
                }
            }
        }

        true
    }
}

#[cfg(windows)]
fn set_startup_enabled_win(enabled: bool) -> anyhow::Result<()> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, KEY_WRITE, REG_BINARY, REG_SZ,
    };

    unsafe {
        let run_sub = to_wide(RUN_KEY_PATH);
        let apprv_sub = to_wide(STARTUP_APPROVED_KEY_PATH);
        let val_name = to_wide(APP_NAME);

        if enabled {
            let current_exe = std::env::current_exe()?;
            let cmd_str = format!("\"{}\" --autostart", current_exe.to_string_lossy());
            let wide_cmd = to_wide(&cmd_str);

            // 1. Write to HKCU\...\Run
            let mut h_run: HKEY = null_mut();
            let res = RegCreateKeyW(
                HKEY_CURRENT_USER,
                run_sub.as_ptr(),
                &mut h_run,
            );
            if res != ERROR_SUCCESS {
                anyhow::bail!("Failed to open/create Windows Run registry key: error code {}", res);
            }

            let cmd_bytes_len = (wide_cmd.len() * std::mem::size_of::<u16>()) as u32;
            let set_res = RegSetValueExW(
                h_run,
                val_name.as_ptr(),
                0,
                REG_SZ,
                wide_cmd.as_ptr() as *const u8,
                cmd_bytes_len,
            );
            RegCloseKey(h_run);

            if set_res != ERROR_SUCCESS {
                anyhow::bail!("Failed to write Windows Run registry value: error code {}", set_res);
            }

            // 2. Write 12-byte enabled marker to HKCU\...\StartupApproved\Run so Task Manager displays "Enabled"
            let mut h_apprv: HKEY = null_mut();
            if RegCreateKeyW(
                HKEY_CURRENT_USER,
                apprv_sub.as_ptr(),
                &mut h_apprv,
            ) == ERROR_SUCCESS
            {
                // Standard 12-byte StartupApproved enabled marker (0x02 = enabled)
                let enabled_marker: [u8; 12] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
                let _ = RegSetValueExW(
                    h_apprv,
                    val_name.as_ptr(),
                    0,
                    REG_BINARY,
                    enabled_marker.as_ptr(),
                    enabled_marker.len() as u32,
                );
                RegCloseKey(h_apprv);
            }
        } else {
            // 1. Remove from HKCU\...\Run
            let mut h_run: HKEY = null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, run_sub.as_ptr(), 0, KEY_WRITE | KEY_SET_VALUE, &mut h_run) == ERROR_SUCCESS {
                let _ = RegDeleteValueW(h_run, val_name.as_ptr());
                RegCloseKey(h_run);
            }

            // 2. Remove from HKCU\...\StartupApproved\Run
            let mut h_apprv: HKEY = null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, apprv_sub.as_ptr(), 0, KEY_WRITE | KEY_SET_VALUE, &mut h_apprv) == ERROR_SUCCESS {
                let _ = RegDeleteValueW(h_apprv, val_name.as_ptr());
                RegCloseKey(h_apprv);
            }
        }
    }

    Ok(())
}
