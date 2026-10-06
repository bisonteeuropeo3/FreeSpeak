//! Windows backend.

use super::{Hotkey, SettingsInput, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
    ERROR_ALREADY_EXISTS,
};
use windows::Win32::Graphics::Gdi::{
    GetStockObject, SetBkMode, COLOR_BTNFACE, DEFAULT_GUI_FONT, HBRUSH, HDC, TRANSPARENT,
};
use windows::Win32::System::Console::{AttachConsole, GetConsoleWindow, ATTACH_PARENT_PROCESS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, SendInput, SetFocus, HOT_KEY_MODIFIERS, INPUT, INPUT_0,
    INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MOD_ALT as WIN_MOD_ALT,
    MOD_CONTROL as WIN_MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT as WIN_MOD_SHIFT,
    MOD_WIN as WIN_MOD_WIN, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FindWindowW, GetDlgItem, GetMessageW, GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW,
    IsDialogMessageW, LoadCursorW, MessageBoxW, PostQuitMessage, RegisterClassW, SendMessageW,
    SetForegroundWindow, ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX,
    BS_DEFPUSHBUTTON, CS_HREDRAW, CS_VREDRAW, ES_AUTOHSCROLL, HMENU, IDC_ARROW, MB_ICONERROR,
    MB_OK, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_HOTKEY, WM_SETFONT, WNDCLASSW, WS_CAPTION,
    WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
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
/// `FREESPEAK_NO_DIALOG=1` suppresses it, which keeps scripted runs from
/// blocking on a modal box. The pre-rename name still works.
pub fn alert(title: &str, message: &str) {
    if std::env::var_os("FREESPEAK_NO_DIALOG").is_some()
        || std::env::var_os("VOICE_NOT_NO_DIALOG").is_some()
    {
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
    Ok(format!("FreeSpeak will start at every login (HKCU\\...\\Run\\{})", super::APP_NAME))
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

/// The single-instance lock, released when this value is dropped.
///
/// A plain `HANDLE` is `Copy`, so `drop(guard)` would not have closed anything
/// and the lock would have stayed held for the life of the process - which is
/// exactly wrong for the settings window, whose job includes *releasing* the lock
/// so a background copy can take it.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Claims the single-instance lock. `Ok(None)` means another copy already holds
/// it; an error means the lock could not be created at all, which must not be
/// reported as "already running".
pub fn single_instance() -> Result<Option<InstanceGuard>, String> {
    let name = wide_z("freespeak-single-instance");
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr()))
            .map_err(|e| format!("could not create the single-instance lock: {e}"))?;
        // CreateMutexW succeeds either way; the error tells us who got there first.
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return Ok(None);
        }
        Ok(Some(InstanceGuard(handle)))
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

// ------------------------------------------------------------- settings window
//
// A real window with two controls, built from raw Win32 rather than a GUI
// toolkit: eframe/egui would add megabytes to a 570 KB binary, and the whole
// point of this app is that it is invisible and weightless until you ask for it.
//
// It runs in its own short-lived process, which is what makes the rest simple:
// no IPC with the background instance is needed, because the daemon re-reads the
// config file on every hotkey press.

/// Control ids. The two buttons double as the ids the dialog manager uses for
/// Enter (default button) and Esc (cancel).
const ID_KEY: i32 = 100;
const ID_SOUND: i32 = 101;
const ID_SAVE: i32 = 1;
const ID_CANCEL: i32 = 2;

/// Window class and title, in one place because finding the window again needs
/// exactly the same pair.
const SETTINGS_CLASS: &str = "FreeSpeakSettings";
const SETTINGS_TITLE: &str = "FreeSpeak settings";

/// `EM_SETSEL`: selects a range in an edit control.
const EM_SETSEL: u32 = 0x00b1;

/// Written by the window procedure, read after the message loop ends.
#[derive(Default)]
struct Outcome {
    saved: bool,
    api_key: String,
    beep: bool,
}

static OUTCOME: OnceLock<Mutex<Outcome>> = OnceLock::new();

fn outcome() -> &'static Mutex<Outcome> {
    OUTCOME.get_or_init(|| Mutex::new(Outcome::default()))
}

fn window_text(hwnd: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buffer);
        let copied = copied.clamp(0, buffer.len() as i32) as usize;
        String::from_utf16_lossy(&buffer[..copied])
    }
}

unsafe extern "system" fn settings_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            match (wparam.0 & 0xffff) as i32 {
                ID_SAVE => {
                    let key = GetDlgItem(hwnd, ID_KEY).unwrap_or_default();
                    let sound = GetDlgItem(hwnd, ID_SOUND).unwrap_or_default();
                    let checked =
                        SendMessageW(sound, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 as u32
                            == BST_CHECKED.0;
                    if let Ok(mut outcome) = outcome().lock() {
                        outcome.saved = true;
                        outcome.api_key = window_text(key);
                        outcome.beep = checked;
                    }
                    let _ = DestroyWindow(hwnd);
                }
                ID_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        // Static controls otherwise paint their own grey rectangle over the
        // window background, which is what makes hand-built Win32 dialogs look
        // broken. Transparent text over the parent's own brush fixes it.
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            SetBkMode(hdc, TRANSPARENT);
            LRESULT((COLOR_BTNFACE.0 + 1) as isize)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// Whether a settings window is already on screen.
///
/// Used by the background copy when it starts without an API key: opening a
/// second window on top of the one the user is already typing into (or the one
/// the installer just opened) is the kind of duplicate that makes an app feel
/// broken.
pub fn settings_window_open() -> bool {
    let class = wide_z(SETTINGS_CLASS);
    let title = wide_z(SETTINGS_TITLE);
    unsafe {
        // FindWindowW returns an error when nothing matches.
        FindWindowW(PCWSTR(class.as_ptr()), PCWSTR(title.as_ptr()))
            .map(|window| window != HWND::default())
            .unwrap_or(false)
    }
}

/// Shows the settings window and waits for it to close.
///
/// `Ok(None)` means the user cancelled, which is not an error.
pub fn show_settings(current: &crate::Config) -> Result<Option<SettingsInput>, String> {
    unsafe {
        // Crisp text on scaled displays. Without this, Windows bitmap-stretches
        // the whole window and it looks broken at 125% and 150%.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let dpi = GetDpiForSystem().max(96) as i32;
        let scale = |value: i32| value * dpi / 96;

        if let Ok(mut outcome) = outcome().lock() {
            *outcome = Outcome::default();
        }

        let instance = GetModuleHandleW(None)
            .map_err(|e| format!("could not reach this program's module handle: {e}"))?;

        let class_name = wide_z(SETTINGS_CLASS);
        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(settings_proc),
            hInstance: HINSTANCE(instance.0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // The classic dialog face, which is what the buttons and the
            // checkbox paint themselves with: matching it makes them blend
            // instead of sitting on differently coloured rectangles.
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        // A second call in the same process fails with "class exists"; the class
        // is already registered and usable either way.
        RegisterClassW(&window_class);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut client = RECT {
            left: 0,
            top: 0,
            right: scale(470),
            bottom: scale(158),
        };
        let _ = AdjustWindowRect(&mut client, style, false);
        let (width, height) = (client.right - client.left, client.bottom - client.top);
        let x = ((GetSystemMetrics(SM_CXSCREEN) - width).max(0)) / 2;
        let y = ((GetSystemMetrics(SM_CYSCREEN) - height).max(0)) / 2;

        let title = wide_z(SETTINGS_TITLE);
        let hwnd = CreateWindowExW(
            // CONTROLPARENT is what lets the dialog manager walk the controls.
            WS_EX_CONTROLPARENT,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            x,
            y,
            width,
            height,
            HWND::default(),
            HMENU::default(),
            instance,
            None,
        )
        .map_err(|e| format!("could not open the settings window: {e}"))?;

        let font = GetStockObject(DEFAULT_GUI_FONT);
        let child = |kind: &str,
                     text: &str,
                     style: WINDOW_STYLE,
                     extra: WINDOW_EX_STYLE,
                     left: i32,
                     top: i32,
                     wide: i32,
                     high: i32,
                     id: i32|
         -> Result<HWND, String> {
            let kind = wide_z(kind);
            let text = wide_z(text);
            let child = CreateWindowExW(
                extra,
                PCWSTR(kind.as_ptr()),
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                scale(left),
                scale(top),
                scale(wide),
                scale(high),
                hwnd,
                HMENU(id as *mut core::ffi::c_void),
                instance,
                None,
            )
            .map_err(|e| format!("could not build the settings window: {e}"))?;
            SendMessageW(child, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            Ok(child)
        };

        child(
            "STATIC",
            "API key",
            WINDOW_STYLE(0),
            WINDOW_EX_STYLE(0),
            16,
            21,
            90,
            20,
            0,
        )?;
        let edit = child(
            "EDIT",
            &current.api_key,
            WINDOW_STYLE(ES_AUTOHSCROLL as u32 | WS_TABSTOP.0),
            WS_EX_CLIENTEDGE,
            112,
            16,
            340,
            24,
            ID_KEY,
        )?;
        child(
            "BUTTON",
            "Play a sound when recording starts and stops",
            WINDOW_STYLE(BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0),
            WINDOW_EX_STYLE(0),
            16,
            58,
            430,
            22,
            ID_SOUND,
        )?;
        // What the line under the checkbox says depends on whether there is
        // anything to fix: an empty key is the one thing that stops this app
        // working at all, and the window that fixes it should say so.
        let hint = if current.api_key.trim().is_empty() {
            "Paste your API key here and press Save: that is all FreeSpeak needs."
        } else {
            "Hotkey, language, provider and the tones live in the config file."
        };
        child(
            "STATIC",
            hint,
            WINDOW_STYLE(0),
            WINDOW_EX_STYLE(0),
            16,
            90,
            430,
            20,
            0,
        )?;
        child(
            "BUTTON",
            "Save",
            WINDOW_STYLE(BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0),
            WINDOW_EX_STYLE(0),
            336,
            118,
            100,
            28,
            ID_SAVE,
        )?;
        child(
            "BUTTON",
            "Cancel",
            WINDOW_STYLE(WS_TABSTOP.0),
            WINDOW_EX_STYLE(0),
            244,
            118,
            84,
            28,
            ID_CANCEL,
        )?;

        let sound = GetDlgItem(hwnd, ID_SOUND).unwrap_or_default();
        let state = if current.beep {
            BST_CHECKED
        } else {
            BST_UNCHECKED
        };
        SendMessageW(sound, BM_SETCHECK, WPARAM(state.0 as usize), LPARAM(0));

        // Start in the key field with the old key selected: replacing it is then
        // one paste.
        let _ = SetFocus(edit);
        SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            // Real dialog behaviour: Tab between the controls, Enter saves, Esc
            // cancels.
            if !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        let outcome = outcome()
            .lock()
            .map_err(|_| "the settings window could not report what you chose".to_string())?;
        if outcome.saved {
            Ok(Some(SettingsInput {
                api_key: outcome.api_key.trim().to_string(),
                beep: outcome.beep,
            }))
        } else {
            Ok(None)
        }
    }
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
