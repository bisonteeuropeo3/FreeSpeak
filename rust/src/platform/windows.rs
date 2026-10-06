//! Windows backend.

use super::{Hotkey, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Console::{AttachConsole, GetConsoleWindow, ATTACH_PARENT_PROCESS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, SendInput, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MOD_ALT as WIN_MOD_ALT,
    MOD_CONTROL as WIN_MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT as WIN_MOD_SHIFT,
    MOD_WIN as WIN_MOD_WIN, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MessageBoxW, MB_ICONERROR, MB_OK, MSG, WM_HOTKEY,
};

/// UTF-16 with a terminating NUL.
fn wide_z(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn now_hms() -> String {
    let time = unsafe { GetLocalTime() };
    format!("{:02}:{:02}:{:02}", time.wHour, time.wMinute, time.wSecond)
}

/// Maps a key name to a Windows virtual key code.
pub fn key_code(name: &str) -> Result<u32, String> {
    let mut chars = name.chars();
    if let (Some(first), None) = (chars.next(), chars.next()) {
        if first.is_ascii_alphabetic() {
            return Ok(first.to_ascii_uppercase() as u32);
        }
        if first.is_ascii_digit() {
            return Ok(first as u32);
        }
    }

    match name {
        "space" => Ok(0x20),
        "tab" => Ok(0x09),
        "enter" | "return" => Ok(0x0D),
        "esc" | "escape" => Ok(0x1B),
        "backspace" => Ok(0x08),
        "insert" => Ok(0x2D),
        "delete" | "del" => Ok(0x2E),
        "home" => Ok(0x24),
        "end" => Ok(0x23),
        "pageup" => Ok(0x21),
        "pagedown" => Ok(0x22),
        "up" => Ok(0x26),
        "down" => Ok(0x28),
        "left" => Ok(0x25),
        "right" => Ok(0x27),
        other => {
            if let Some(digits) = other.strip_prefix('f') {
                if let Ok(number) = digits.parse::<u32>() {
                    if (1..=24).contains(&number) {
                        return Ok(0x70 + number - 1);
                    }
                }
            }
            Err(format!(
                "unknown key '{other}' (use a-z, 0-9, space, tab, enter, esc, f1-f12)"
            ))
        }
    }
}

fn win_modifiers(modifiers: u32) -> HOT_KEY_MODIFIERS {
    let mut flags = MOD_NOREPEAT;
    if modifiers & MOD_CTRL != 0 {
        flags |= WIN_MOD_CONTROL;
    }
    if modifiers & MOD_ALT != 0 {
        flags |= WIN_MOD_ALT;
    }
    if modifiers & MOD_SHIFT != 0 {
        flags |= WIN_MOD_SHIFT;
    }
    if modifiers & MOD_META != 0 {
        flags |= WIN_MOD_WIN;
    }
    flags
}

/// The release build is a windowless binary, so a terminal-launched copy has no
/// console of its own. Attaching to the terminal that started us keeps `--help`
/// and friends printing where the user can read them. Started by Windows there
/// is no parent console and this does nothing.
pub fn bind_parent_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// True when we have a console to print to and read from.
pub fn has_console() -> bool {
    unsafe { !GetConsoleWindow().0.is_null() }
}

pub fn register_hotkey(id: i32, hotkey: &Hotkey) -> Result<(), String> {
    unsafe {
        RegisterHotKey(None, id, win_modifiers(hotkey.modifiers), hotkey.code)
            .map_err(|e| e.to_string())
    }
}

/// Blocks in a Win32 message loop, calling `on_hotkey` with the hotkey id.
pub fn run_message_loop<F: FnMut(i32)>(mut on_hotkey: F) {
    let mut message = MSG::default();
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            if message.message == WM_HOTKEY {
                on_hotkey(message.wParam.0 as i32);
            }
        }
    }
}

/// Dialog, for errors that a windowless background instance cannot print.
///
/// `VOICE_NOT_NO_DIALOG=1` suppresses it, which keeps scripted runs from
/// blocking on a modal box.
pub fn alert(title: &str, message: &str) {
    if std::env::var_os("VOICE_NOT_NO_DIALOG").is_some() {
        return;
    }
    let title = wide_z(title);
    let message = wide_z(message);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Reads one line from the console, for the first-run API key prompt.
pub fn read_line() -> Option<String> {
    use std::io::BufRead;
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line.trim().to_string()),
        Err(_) => None,
    }
}

/// Opens a file in the user's default editor.
pub fn open_in_editor(path: &Path) -> Result<(), String> {
    std::process::Command::new("notepad")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open {}: {e}", path.display()))
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Registers the current executable to start at login, via the per-user Run key.
pub fn install_autostart() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("could not find the executable: {e}"))?;
    let output = std::process::Command::new("reg")
        .args([
            "add",
            RUN_KEY,
            "/v",
            super::APP_NAME,
            "/t",
            "REG_SZ",
            "/d",
            &format!("\"{}\"", exe.display()),
            "/f",
        ])
        .output()
        .map_err(|e| format!("could not run reg.exe: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(format!("Voice Not will start at every login (HKCU\\...\\Run\\{})", super::APP_NAME))
}

pub fn uninstall_autostart() -> Result<String, String> {
    let output = std::process::Command::new("reg")
        .args(["delete", RUN_KEY, "/v", super::APP_NAME, "/f"])
        .output()
        .map_err(|e| format!("could not run reg.exe: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("unable to find") || stderr.contains("Impossibile trovare") {
            return Ok("there was no login entry to remove".to_string());
        }
        return Err(stderr.trim().to_string());
    }
    Ok("removed the login entry".to_string())
}

/// Claims the single-instance lock. `Ok(None)` means another copy already holds
/// it; an error means the lock could not be created at all, which must not be
/// reported as "already running".
pub fn single_instance() -> Result<Option<HANDLE>, String> {
    let name = wide_z("voice-not-single-instance");
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr()))
            .map_err(|e| format!("could not create the single-instance lock: {e}"))?;
        // CreateMutexW succeeds either way; the error tells us who got there first.
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return Ok(None);
        }
        Ok(Some(handle))
    }
}

/// Copies `text` to the clipboard, retrying while another process holds it open.
pub fn set_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("opening the clipboard: {e}"))?;
    let mut last_error = String::new();
    for attempt in 0..12u64 {
        match clipboard.set_text(text.to_string()) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = err.to_string();
                std::thread::sleep(Duration::from_millis(30 * attempt.min(4) + 20));
            }
        }
    }
    Err(format!(
        "another program is holding the clipboard ({last_error})"
    ))
}

fn key_event(key: VIRTUAL_KEY, release: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: if release {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) < 0 }
}

/// Sends Ctrl+V to whichever window has focus.
///
/// Waits for the user to let go of the modifiers first: the stop hotkey is
/// usually Ctrl+Alt+Space, and pasting while Alt is still held would send
/// Ctrl+Alt+V to the target application instead.
pub fn send_paste() -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(2000);
    while Instant::now() < deadline {
        let held = key_down(VK_CONTROL.0 as i32)
            || key_down(VK_MENU.0 as i32)
            || key_down(VK_SHIFT.0 as i32)
            || key_down(VK_LWIN.0 as i32)
            || key_down(VK_RWIN.0 as i32);
        if !held {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    let inputs = [
        key_event(VK_CONTROL, false),
        key_event(VK_V, false),
        key_event(VK_V, true),
        key_event(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == 0 {
        return Err("SendInput was refused; the focused window may be running elevated".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_key_names_to_virtual_keys() {
        assert_eq!(key_code("space").unwrap(), 0x20);
        assert_eq!(key_code("f9").unwrap(), 0x78);
        assert_eq!(key_code("d").unwrap(), 'D' as u32);
        assert_eq!(key_code("0").unwrap(), '0' as u32);
        assert!(key_code("banana").is_err());
        assert!(key_code("f99").is_err());
    }
}
