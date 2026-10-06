//! macOS backend.
//!
//! Notes on the choices here, since this file cannot be exercised on Windows:
//!
//! * Hotkeys use Carbon's `RegisterEventHotKey`. Unlike a `CGEventTap`, that
//!   needs no Accessibility permission, so dictation works out of the box.
//! * Pasting does need Accessibility permission (any synthetic keystroke does).
//!   When it is missing the user is told, and sent to the right settings pane.
//! * Autostart is a per-user LaunchAgent in ~/Library/LaunchAgents.
//! * Alerts go through `osascript`, which needs no extra permissions.

use super::{
    applescript_string, inside_app_bundle, launch_agent_plist, Hotkey, SettingsInput, MOD_ALT,
    MOD_CTRL, MOD_META, MOD_SHIFT,
};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- Carbon glue
//
// `RunApplicationEventLoop` keeps the process alive and dispatches hotkeys. The
// process is transformed into a background-only app so it never shows in the
// Dock or the app switcher.

type OSStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct ProcessSerialNumber {
    high: u32,
    low: u32,
}

const K_EVENT_CLASS_KEYBOARD: u32 = 0x6B65_7962; // 'keyb'
const K_EVENT_HOTKEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = 0x2D2D_2D2D; // '----'
const TYPE_EVENT_HOTKEY_ID: u32 = 0x686B_6964; // 'hkid'
const HOTKEY_SIGNATURE: u32 = 0x564E_4F54; // 'FSPK'
const K_PROCESS_TRANSFORM_TO_BACKGROUND_APPLICATION: u32 = 2;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus,
        num_types: u32,
        type_list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: u32,
        actual_size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
    fn RunApplicationEventLoop();
    fn TransformProcessType(psn: *const ProcessSerialNumber, flags: u32) -> OSStatus;
    fn GetCurrentProcess(psn: *mut ProcessSerialNumber) -> OSStatus;
}

/// Carbon modifier masks.
const CARBON_CMD: u32 = 1 << 8;
const CARBON_SHIFT: u32 = 1 << 9;
const CARBON_OPTION: u32 = 1 << 11;
const CARBON_CONTROL: u32 = 1 << 12;

static HANDLER: OnceLock<Mutex<Box<dyn FnMut(i32) + Send>>> = OnceLock::new();

extern "C" fn hotkey_handler(
    _call_ref: EventHandlerCallRef,
    event: EventRef,
    _user_data: *mut c_void,
) -> OSStatus {
    let mut hotkey_id = EventHotKeyID {
        signature: HOTKEY_SIGNATURE,
        id: 0,
    };
    let mut actual_type = 0u32;
    let mut actual_size = 0u32;
    let status = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOTKEY_ID,
            &mut actual_type,
            std::mem::size_of::<EventHotKeyID>() as u32,
            &mut actual_size,
            &mut hotkey_id as *mut EventHotKeyID as *mut c_void,
        )
    };
    if status == 0 {
        if let Some(handler) = HANDLER.get() {
            if let Ok(mut callback) = handler.lock() {
                callback(hotkey_id.id as i32);
            }
        }
    }
    0 // noErr
}

// ------------------------------------------------------- CoreGraphics paste

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventFlags = u64;

const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION: i32 = 0;
const K_CG_HID_EVENT_TAP: u32 = 0;
const CG_FLAG_SHIFT: CGEventFlags = 0x0002_0000;
const CG_FLAG_CONTROL: CGEventFlags = 0x0004_0000;
const CG_FLAG_ALTERNATE: CGEventFlags = 0x0008_0000;
const CG_FLAG_COMMAND: CGEventFlags = 0x0010_0000;

/// macOS virtual key code for `v`.
const KEY_V: u16 = 9;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSourceFlagsState(state_id: i32) -> CGEventFlags;
    fn CFRelease(cf: *const c_void);
    fn AXIsProcessTrusted() -> bool;
}

// ------------------------------------------------------------------ libc bits

#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const i8,
}

extern "C" {
    fn time(timer: *mut i64) -> i64;
    fn localtime_r(timer: *const i64, result: *mut Tm) -> *mut Tm;
    fn isatty(fd: i32) -> i32;
    fn flock(fd: i32, operation: i32) -> i32;
    fn getuid() -> u32;
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

pub fn now_hms() -> String {
    let mut now: i64 = 0;
    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    unsafe {
        time(&mut now);
        if localtime_r(&now, &mut tm).is_null() {
            return "??:??:??".to_string();
        }
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

// --------------------------------------------------------------- hotkey table

/// macOS virtual key codes (ANSI layout).
pub fn key_code(name: &str) -> Result<u32, String> {
    let code = match name {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "9" => 25,
        "7" => 26,
        "8" => 28,
        "0" => 29,
        "o" => 31,
        "u" => 32,
        "i" => 34,
        "p" => 35,
        "l" => 37,
        "j" => 38,
        "k" => 40,
        "n" => 45,
        "m" => 46,
        "space" => 49,
        "tab" => 48,
        "enter" | "return" => 36,
        "esc" | "escape" => 53,
        "backspace" | "delete" => 51,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        other => {
            return Err(format!(
                "unknown key '{other}' (use a-z, 0-9, space, tab, enter, esc, f1-f12)"
            ))
        }
    };
    Ok(code)
}

fn carbon_modifiers(modifiers: u32) -> u32 {
    let mut flags = 0;
    if modifiers & MOD_CTRL != 0 {
        flags |= CARBON_CONTROL;
    }
    if modifiers & MOD_ALT != 0 {
        flags |= CARBON_OPTION;
    }
    if modifiers & MOD_SHIFT != 0 {
        flags |= CARBON_SHIFT;
    }
    if modifiers & MOD_META != 0 {
        flags |= CARBON_CMD;
    }
    flags
}

pub fn register_hotkey(id: i32, hotkey: &Hotkey) -> Result<(), String> {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.get().is_none() {
        let spec = EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOTKEY_PRESSED,
        };
        let mut handler_ref: EventHandlerRef = std::ptr::null_mut();
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                hotkey_handler,
                1,
                &spec,
                std::ptr::null_mut(),
                &mut handler_ref,
            )
        };
        if status != 0 {
            return Err(format!("InstallEventHandler failed (status {status})"));
        }
        let _ = INSTALLED.set(());
    }

    let mut hotkey_ref: EventHotKeyRef = std::ptr::null_mut();
    let modifiers = carbon_modifiers(hotkey.modifiers);
    // Carbon rejects a hot key with no modifier, or with shift alone, before it
    // even looks at whether another app owns it. Say so, instead of letting the
    // caller report "someone else has that combination".
    if modifiers & !CARBON_SHIFT == 0 {
        return Err(
            "macOS needs at least one of ctrl, alt or cmd in a hot key (shift alone is not \
             enough)"
                .to_string(),
        );
    }
    let status = unsafe {
        RegisterEventHotKey(
            hotkey.code,
            modifiers,
            EventHotKeyID {
                signature: HOTKEY_SIGNATURE,
                id: id as u32,
            },
            GetApplicationEventTarget(),
            0,
            &mut hotkey_ref,
        )
    };
    match status {
        0 => Ok(()),
        // eventHotKeyExistsErr
        -9878 => Err("another application already owns that hot key".to_string()),
        other => Err(format!("macOS refused that hot key (error {other})")),
    }
}

/// Runs the Carbon event loop, dispatching hotkey presses to `on_hotkey`.
pub fn run_message_loop<F: FnMut(i32) + Send + 'static>(callback: F) {
    let _ = HANDLER.set(Mutex::new(Box::new(callback)));

    // Background-only: no Dock icon, no app switcher entry.
    unsafe {
        let mut psn = ProcessSerialNumber { high: 0, low: 0 };
        if GetCurrentProcess(&mut psn) == 0 {
            let _ = TransformProcessType(&psn, K_PROCESS_TRANSFORM_TO_BACKGROUND_APPLICATION);
        }
        RunApplicationEventLoop();
    }
}

// ------------------------------------------------------------ console & misc

/// macOS has no attached-console concept: a terminal launch is a tty.
///
/// Both ends are checked because the two uses differ - the API key prompt reads
/// **stdin**, while everything else writes to stdout - and asking either one on
/// its own gets the other case wrong. `freespeak | tee log` at a terminal can
/// still be typed into; a launchd job has neither and gets a dialog instead.
pub fn has_console() -> bool {
    unsafe { isatty(0) == 1 || isatty(1) == 1 }
}

/// Nothing to do on macOS; the binary never creates a window.
pub fn bind_parent_console() {}

pub fn read_line() -> Option<String> {
    use std::io::BufRead;
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line.trim().to_string()),
        Err(_) => None,
    }
}

pub fn alert(title: &str, message: &str) {
    if std::env::var_os("FREESPEAK_NO_DIALOG").is_some()
        || std::env::var_os("VOICE_NOT_NO_DIALOG").is_some()
    {
        return;
    }
    let script = format!(
        "display dialog {} with title {} buttons {{\"OK\"}} default button \"OK\" with icon caution",
        applescript_string(message),
        applescript_string(title)
    );
    // A failed dialog must not be silent as well: the log always gets a line,
    // and osascript's own complaint is passed on rather than dropped.
    match Command::new("osascript").arg("-e").arg(script).output() {
        Ok(output) if !output.status.success() => {
            crate::logging::line(&format!(
                "could not show a dialog (osascript: {}); the message was: {}",
                String::from_utf8_lossy(&output.stderr).trim(),
                message.replace('\n', " ")
            ));
        }
        Err(err) => {
            crate::logging::line(&format!(
                "could not run osascript ({err}); the message was: {}",
                message.replace('\n', " ")
            ));
        }
        Ok(_) => {}
    }
}

pub fn open_in_editor(path: &Path) -> Result<(), String> {
    Command::new("open")
        .arg("-t")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open {}: {e}", path.display()))
}

// ------------------------------------------------------------------ clipboard

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
    Err(format!("could not write to the clipboard ({last_error})"))
}

/// Sends Cmd+V to whichever application has focus.
///
/// Synthetic keystrokes require Accessibility permission, and macOS drops them
/// silently without it. `AXIsProcessTrusted` only reads the decision - it does
/// not create the entry - so the message spells out the one-time manual step,
/// with the path to add, instead of naming a row that does not exist yet.
pub fn send_paste() -> Result<(), String> {
    if !unsafe { AXIsProcessTrusted() } {
        let _ = Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
        let me = std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "FreeSpeak".to_string());
        return Err(format!(
            "macOS is blocking synthetic keystrokes, so the transcript was copied to the \
             clipboard instead of pasted.\n\nFix it once in System Settings > Privacy & \
             Security > Accessibility: click + and choose\n\n    {me}\n\nAn app bundle keeps \
             that permission across updates; a bare binary in target/ loses it on every rebuild."
        ));
    }

    // Wait for the modifier keys to be released: the stop hotkey is usually
    // Ctrl+Alt+Space, and pasting while they are held would send Cmd+Alt+V.
    let deadline = Instant::now() + Duration::from_millis(2000);
    while Instant::now() < deadline {
        let held = unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) }
            & (CG_FLAG_COMMAND | CG_FLAG_ALTERNATE | CG_FLAG_CONTROL | CG_FLAG_SHIFT);
        if held == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION);
        for pressed in [true, false] {
            let event = CGEventCreateKeyboardEvent(source, KEY_V, pressed);
            if event.is_null() {
                if !source.is_null() {
                    CFRelease(source as *const c_void);
                }
                return Err("could not create a keyboard event".to_string());
            }
            CGEventSetFlags(event, CG_FLAG_COMMAND);
            CGEventPost(K_CG_HID_EVENT_TAP, event);
            CFRelease(event as *const c_void);
        }
        if !source.is_null() {
            CFRelease(source as *const c_void);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ settings
//
// Two `osascript` dialogs rather than a Cocoa window. A real one means several
// hundred lines of Objective-C runtime calls, and unlike the rest of this file it
// cannot be type-checked anywhere but a Mac: a mistake is a crash, not a bad
// dialog. The dialogs are native, modal and keyboard-driven, which is what the
// two settings actually need.

fn run_osascript(script: &str) -> Result<Option<String>, String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("could not run osascript: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        // Cancelling makes osascript exit non-zero; that is a choice, not a fault.
        if stderr.contains("User canceled") || stderr.contains("User cancelled") {
            return Ok(None);
        }
        return Err(format!("osascript said: {}", stderr.trim()));
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
}

/// Pulls `field:value` out of osascript's `button returned:Save, text returned:x`.
fn osascript_field(reply: &str, field: &str) -> Option<String> {
    let needle = format!("{field}:");
    let start = reply.find(&needle)? + needle.len();
    let rest = &reply[start..];
    let end = rest.find(", ").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// Shows the settings and waits for them, `Ok(None)` when cancelled.
pub fn show_settings(current: &crate::Config) -> Result<Option<SettingsInput>, String> {
    let key_script = format!(
        "display dialog \"The API key FreeSpeak uses to transcribe.\" \
         with title \"FreeSpeak settings\" default answer {} \
         buttons {{\"Cancel\", \"Next\"}} default button \"Next\" with hidden answer",
        applescript_string(&current.api_key)
    );
    let reply = match run_osascript(&key_script)? {
        Some(reply) => reply,
        None => return Ok(None),
    };
    let api_key = osascript_field(&reply, "text returned").unwrap_or_default();
    if api_key.trim().is_empty() {
        return Err("the API key cannot be empty: without it nothing can be transcribed".to_string());
    }

    let default = if current.beep { "Sound" } else { "Silent" };
    let sound_script = format!(
        "display dialog \"Play a sound when recording starts and stops?\" \
         with title \"FreeSpeak settings\" buttons {{\"Silent\", \"Sound\"}} default button \"{default}\""
    );
    let reply = match run_osascript(&sound_script)? {
        Some(reply) => reply,
        None => return Ok(None),
    };
    let beep = osascript_field(&reply, "button returned")
        .map(|button| button == "Sound")
        .unwrap_or(current.beep);

    Ok(Some(SettingsInput { api_key, beep }))
}

// ----------------------------------------------------------- single instance

/// Holds an exclusive `flock` on a lock file; released when the process exits,
/// so a crash cannot leave a stale lock behind.
pub struct InstanceGuard(#[allow(dead_code)] std::fs::File);

pub fn single_instance() -> Result<Option<InstanceGuard>, String> {
    use std::os::unix::io::AsRawFd;

    let path = crate::config::data_dir().join("freespeak.lock");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("could not open the lock file {}: {e}", path.display()))?;
    let locked = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0;
    if locked {
        Ok(Some(InstanceGuard(file)))
    } else {
        Ok(None)
    }
}

// ------------------------------------------------------------------ autostart

const LAUNCH_AGENT: &str = "com.freespeak.plist";

fn launch_agent_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(LAUNCH_AGENT))
}

pub fn install_autostart() -> Result<String, String> {
    let exe = stable_exe()?;
    let path = launch_agent_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    // Only stderr goes to a file: the app writes its own log, and pointing both
    // at the same one duplicated every line.
    let stderr_log = crate::config::data_dir().join("launchd.log");
    let plist = launch_agent_plist(&exe, &crate::config::log_path(), &stderr_log);
    std::fs::write(&path, plist).map_err(|e| format!("could not write {}: {e}", path.display()))?;

    // A malformed property list is rejected by launchd with a message that is
    // easy to miss, so check the file parses before believing the install.
    if let Ok(output) = Command::new("plutil").arg("-lint").arg(&path).output() {
        if !output.status.success() {
            return Err(format!(
                "the login entry at {} is not a valid property list: {}",
                path.display(),
                String::from_utf8_lossy(&output.stdout).trim()
            ));
        }
    }

    load_agent(&path)?;
    Ok(format!(
        "FreeSpeak will start at every login ({}), launching {}",
        path.display(),
        exe.display()
    ))
}

/// The path the login entry should start.
///
/// Running from a `.app` bundle is already permanent and signable, so it is used
/// as it is. A bare `target/release/freespeak` is not: it disappears with
/// `cargo clean`, which silently killed autostart, so it gets copied next to the
/// config first.
fn stable_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("could not find the executable: {e}"))?;
    if inside_app_bundle(&exe) {
        return Ok(exe);
    }
    let dir = crate::config::data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let installed = dir.join("freespeak");
    std::fs::copy(&exe, &installed).map_err(|e| {
        format!(
            "could not copy {} to {}: {e}",
            exe.display(),
            installed.display()
        )
    })?;
    Ok(installed)
}

/// Loads the login entry, reporting what launchd actually said.
///
/// `bootstrap` is the supported form on macOS 11+; `load -w` still works on
/// older systems and is used as the fallback. Either way the exit status is
/// checked: the previous version discarded it and announced success even when
/// launchd had refused the job.
fn load_agent(path: &Path) -> Result<(), String> {
    let uid = unsafe { getuid() };
    let domain = format!("gui/{uid}");
    let bootstrap = Command::new("launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(path)
        .output();
    match bootstrap {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(output) => {
            // 5 = "Input/output error", which is also what an already-loaded job
            // reports; that is not a failure worth stopping for.
            let complaint = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let legacy = Command::new("launchctl")
                .arg("load")
                .arg("-w")
                .arg(path)
                .output();
            if let Ok(legacy) = legacy {
                if legacy.status.success() {
                    return Ok(());
                }
            }
            return Err(format!(
                "launchd would not load {}: {complaint}",
                path.display()
            ));
        }
        Err(err) => return Err(format!("could not run launchctl: {err}")),
    }
}

pub fn uninstall_autostart() -> Result<String, String> {
    let path = launch_agent_path()?;
    let uid = unsafe { getuid() };
    let domain = format!("gui/{uid}");
    if !path.exists() {
        // Still try to drop a job whose plist was deleted by hand.
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/com.freespeak")])
            .output();
        return Ok("there was no login entry to remove".to_string());
    }

    let bootout = Command::new("launchctl")
        .args(["bootout", &domain])
        .arg(&path)
        .output();
    let stopped = matches!(&bootout, Ok(output) if output.status.success());
    if !stopped {
        let legacy = Command::new("launchctl")
            .arg("unload")
            .arg("-w")
            .arg(&path)
            .output();
        if !matches!(&legacy, Ok(output) if output.status.success()) {
            // The file is going away regardless; say what happened rather than
            // claiming a clean removal.
            std::fs::remove_file(&path)
                .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
            return Ok(format!(
                "removed {} - launchctl did not confirm the job stopped, so it may restart if it is still running",
                path.display()
            ));
        }
    }
    std::fs::remove_file(&path).map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    Ok("removed the login entry".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_key_names_to_mac_keycodes() {
        assert_eq!(key_code("space").unwrap(), 49);
        assert_eq!(key_code("v").unwrap(), 9);
        assert_eq!(key_code("f9").unwrap(), 101);
        assert_eq!(key_code("q").unwrap(), 12);
        assert!(key_code("banana").is_err());
    }

    #[test]
    fn escapes_applescript_strings() {
        assert_eq!(applescript_string(r#"a "b" c"#), r#""a \"b\" c""#);
        assert_eq!(applescript_string(r"back\slash"), r#""back\\slash""#);
    }

    #[test]
    fn carbon_modifier_mapping() {
        assert_eq!(carbon_modifiers(MOD_CTRL), CARBON_CONTROL);
        assert_eq!(carbon_modifiers(MOD_ALT), CARBON_OPTION);
        assert_eq!(carbon_modifiers(MOD_SHIFT), CARBON_SHIFT);
        assert_eq!(carbon_modifiers(MOD_META), CARBON_CMD);
        assert_eq!(
            carbon_modifiers(MOD_CTRL | MOD_ALT),
            CARBON_CONTROL | CARBON_OPTION
        );
    }
}
