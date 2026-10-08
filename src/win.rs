//! Small safe wrappers over the Win32 calls the Windows build needs.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, MAX_PATH, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Registry::{
    HKEY, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
};

pub use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;

/// A NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain([0]).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..len])
        .to_string_lossy()
        .into_owned()
}

/// A string value from the registry, such as Steam's install path.
pub fn registry_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffers are valid for the sizes given.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == 0).then(|| from_wide(&buf))
}

pub fn set_registry_string(root: HKEY, key: &str, value: &str, data: &str) -> std::io::Result<()> {
    let (key, value, data) = (wide(key), wide(value), wide(data));
    // SAFETY: the strings are NUL-terminated and the size covers `data`.
    let status = unsafe {
        RegSetKeyValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    match status {
        0 => Ok(()),
        e => Err(std::io::Error::from_raw_os_error(e as i32)),
    }
}

/// Removes a registry value; `Ok(false)` if it wasn't there.
pub fn delete_registry_value(root: HKEY, key: &str, value: &str) -> std::io::Result<bool> {
    let (key, value) = (wide(key), wide(value));
    // SAFETY: the strings are NUL-terminated.
    match unsafe { RegDeleteKeyValueW(root, key.as_ptr(), value.as_ptr()) } {
        0 => Ok(true),
        2 => Ok(false), // ERROR_FILE_NOT_FOUND
        e => Err(std::io::Error::from_raw_os_error(e as i32)),
    }
}

/// True if a process with this executable name is running.
pub fn process_running(exe: &str) -> bool {
    // SAFETY: the snapshot handle is closed before returning; the entry's size field is set.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == -1 {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut ok = Process32FirstW(snapshot, &mut entry) != 0;
        while ok {
            if from_wide(&entry.szExeFile).eq_ignore_ascii_case(exe) {
                found = true;
                break;
            }
            ok = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        found
    }
}

/// The window in front.
pub struct Foreground {
    pub hwnd: HWND,
    /// Lower-case "<exe name> <title>", for matching against `window_match`.
    pub description: String,
    /// Whether it belongs to this program.
    pub ours: bool,
}

/// Stops background raw mouse input, which winit asks for along with the keyboard. Only the
/// keyboard is needed (the hotkey), and a high-rate gaming mouse would otherwise wake the overlay
/// thousands of times a second during play.
pub fn stop_background_mouse() {
    use windows_sys::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices};
    let mouse = RAWINPUTDEVICE {
        usUsagePage: 1, // generic desktop
        usUsage: 2,     // mouse
        dwFlags: RIDEV_REMOVE,
        hwndTarget: std::ptr::null_mut(),
    };
    // SAFETY: one valid entry, with its size.
    unsafe { RegisterRawInputDevices(&mouse, 1, size_of::<RAWINPUTDEVICE>() as u32) };
}

/// The front window's handle (null if none), without the lookups `foreground` does.
pub fn foreground_window() -> HWND {
    // SAFETY: a plain query.
    unsafe { GetForegroundWindow() }
}

/// Whether `hwnd` still names a window (the game may have closed since it was seen).
pub fn is_window(hwnd: HWND) -> bool {
    // SAFETY: IsWindow accepts any value.
    unsafe { IsWindow(hwnd) != 0 }
}

pub fn foreground() -> Option<Foreground> {
    // SAFETY: plain queries on a window handle; the process handle is closed.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut title = [0u16; 512];
        let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        let title = from_wide(&title[..len.max(0) as usize]);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let mut exe = String::new();
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !process.is_null() {
            let mut path = [0u16; MAX_PATH as usize];
            let mut size = path.len() as u32;
            if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut size)
                != 0
            {
                let path = from_wide(&path[..size as usize]);
                exe = path.rsplit('\\').next().unwrap_or(&path).to_string();
            }
            CloseHandle(process);
        }
        Some(Foreground {
            hwnd,
            description: format!("{exe} {title}").trim().to_lowercase(),
            ours: pid == GetCurrentProcessId(),
        })
    }
}

/// The bounds (in physical pixels) of the monitor showing `hwnd`.
pub fn monitor_rect(hwnd: HWND) -> Option<RECT> {
    // SAFETY: MONITORINFO's size field is set before the call.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info.rcMonitor)
    }
}
