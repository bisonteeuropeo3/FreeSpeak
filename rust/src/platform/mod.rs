//! Platform integration, split by operating system.
//!
//! Everything above this layer is portable; each backend here provides the same
//! small API: a message loop with global hotkeys, the clipboard and paste key, a
//! dialog, console access, opening the config, autostart and a single-instance
//! lock.

use std::fmt;
use std::path::Path;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(any(windows, target_os = "macos")))]
compile_error!(
    "FreeSpeak supports Windows and macOS. A Linux backend needs X11/Wayland hotkey \
     handling; the audio, HTTP and clipboard layers are already portable."
);

pub const APP_NAME: &str = "FreeSpeak";

/// What the settings window collects.
///
/// Kept to the two things worth a control. Everything else in the config file is
/// either set once (hotkey, device, provider) or a matter of taste that the file
/// documents better than a dialog could.
pub struct SettingsInput {
    pub api_key: String,
    pub beep: bool,
}

pub const HOTKEY_TOGGLE: i32 = 1;
pub const HOTKEY_QUIT: i32 = 2;

pub const MOD_CTRL: u32 = 1 << 0;
pub const MOD_ALT: u32 = 1 << 1;
pub const MOD_SHIFT: u32 = 1 << 2;
/// Windows key on Windows, Command on macOS.
pub const MOD_META: u32 = 1 << 3;

/// A parsed hotkey: a platform key code plus modifier bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub code: u32,
    pub modifiers: u32,
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.modifiers & MOD_CTRL != 0 {
            parts.push("ctrl");
        }
        if self.modifiers & MOD_ALT != 0 {
            parts.push("alt");
        }
        if self.modifiers & MOD_SHIFT != 0 {
            parts.push("shift");
        }
        if self.modifiers & MOD_META != 0 {
            parts.push(if cfg!(target_os = "macos") { "cmd" } else { "win" });
        }
        parts.push("key");
        write!(f, "{}", parts.join("+"))
    }
}

/// Shared hotkey parsing. The key table itself is per platform, because a
/// "space" is 0x20 on Windows and 49 on macOS.
pub fn parse_hotkey(spec: &str) -> Result<Hotkey, String> {
    let mut modifiers = 0u32;
    let mut code: Option<u32> = None;

    for part in spec.split('+') {
        let part = part.trim().to_ascii_lowercase();
        match part.as_str() {
            "" => {}
            "ctrl" | "control" => modifiers |= MOD_CTRL,
            "alt" | "option" | "opt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            "win" | "super" | "meta" | "cmd" | "command" => modifiers |= MOD_META,
            other => code = Some(key_code(other)?),
        }
    }

    let code = code.ok_or_else(|| format!("hotkey '{spec}' has no main key"))?;
    Ok(Hotkey { code, modifiers })
}

// ------------------------------------------------- macOS-only text helpers
//
// These live here, rather than in the macOS backend, for one reason: the macOS
// code cannot be compiled on the machine this was written on, and a broken
// property list or a bad AppleScript literal fails *silently* on the user's Mac.
// Keeping the string building in shared code means it is covered by tests that
// actually run.

/// Escapes text for an XML text node. A home directory can legally contain `&`
/// or `<`, and either one turns the login entry into a file launchd rejects.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Quotes a string for use inside an AppleScript literal.
///
/// Line breaks matter as much as quotes here: the messages passed to the alert
/// contain blank lines, and a literal newline inside an AppleScript string is a
/// syntax error. `osascript` then exits without showing anything, which is
/// indistinguishable from "the app crashed". They become `& return &`
/// concatenation instead, which AppleScript accepts.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn applescript_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\r' => {}
            '\n' => quoted.push_str("\" & return & \""),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// The LaunchAgent that starts FreeSpeak at login (`~/Library/LaunchAgents`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn launch_agent_plist(program: &Path, stdout_log: &Path, stderr_log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.freespeak</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        program = xml_escape(&program.display().to_string()),
        stdout = xml_escape(&stdout_log.display().to_string()),
        stderr = xml_escape(&stderr_log.display().to_string()),
    )
}

/// True when `path` sits inside a `.app` bundle.
///
/// An app started from a bundle is the only macOS layout that is both stable
/// (`cargo clean` cannot delete it) and signable, which is what makes the
/// Microphone and Accessibility permissions survive a rebuild. A bare binary in
/// `target/release` is neither, so autostart copies it somewhere permanent.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn inside_app_bundle(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str().to_string_lossy().ends_with(".app"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers() {
        let hotkey = parse_hotkey("ctrl+alt+space").unwrap();
        assert_eq!(hotkey.modifiers, MOD_CTRL | MOD_ALT);

        let all = parse_hotkey("ctrl+alt+shift+cmd+d").unwrap();
        assert_eq!(all.modifiers, MOD_CTRL | MOD_ALT | MOD_SHIFT | MOD_META);
    }

    #[test]
    fn rejects_bad_hotkeys() {
        assert!(parse_hotkey("ctrl+alt").is_err());
        assert!(parse_hotkey("ctrl+banana").is_err());
        assert!(parse_hotkey("f99").is_err());
    }

    #[test]
    fn both_platforms_know_the_default_hotkeys() {
        assert!(parse_hotkey("ctrl+alt+space").is_ok());
        assert!(parse_hotkey("ctrl+alt+shift+q").is_ok());
    }

    #[test]
    fn paths_that_break_xml_are_escaped() {
        assert_eq!(xml_escape("/Users/a&b/log"), "/Users/a&amp;b/log");
        assert_eq!(xml_escape("/Users/<odd>/x"), "/Users/&lt;odd&gt;/x");
        // The common case must come through untouched.
        assert_eq!(
            xml_escape("/Users/olek/Library/Application Support"),
            "/Users/olek/Library/Application Support"
        );
    }

    #[test]
    fn the_login_entry_stays_valid_xml_for_awkward_home_directories() {
        let plist = launch_agent_plist(
            Path::new("/Users/a&b/FreeSpeak.app/Contents/MacOS/freespeak"),
            Path::new("/Users/a&b/Library/dictate.log"),
            Path::new("/Users/a&b/Library/launchd.log"),
        );
        assert!(
            !plist.contains("a&b"),
            "a raw ampersand would make launchd reject the file:\n{plist}"
        );
        assert!(plist.contains("/Users/a&amp;b/FreeSpeak.app/Contents/MacOS/freespeak"));
        // The keys launchd needs to accept the job at all.
        for key in [
            "<key>Label</key>",
            "<key>ProgramArguments</key>",
            "<key>RunAtLoad</key>",
            "<key>StandardOutPath</key>",
            "<key>StandardErrorPath</key>",
        ] {
            assert!(plist.contains(key), "missing {key}");
        }
        // Balanced tags, so the file parses.
        assert_eq!(plist.matches("<dict>").count(), plist.matches("</dict>").count());
        assert_eq!(plist.matches("<array>").count(), plist.matches("</array>").count());
        assert_eq!(plist.matches("<plist").count(), 1);
        assert_eq!(plist.matches("</plist>").count(), 1);
    }

    #[test]
    fn applescript_literals_survive_quotes_and_blank_lines() {
        assert_eq!(applescript_string(r#"a "b" c"#), r#""a \"b\" c""#);
        assert_eq!(applescript_string(r"back\slash"), r#""back\\slash""#);
        // The messages shown by `alert` are written with \n\n.
        assert_eq!(
            applescript_string("first line\n\nsecond line"),
            "\"first line\" & return & \"\" & return & \"second line\""
        );
        assert!(
            !applescript_string("a\nb").contains('\n'),
            "a literal newline is a syntax error in AppleScript"
        );
    }

    #[test]
    fn an_app_bundle_is_recognised() {
        assert!(inside_app_bundle(Path::new(
            "/Applications/FreeSpeak.app/Contents/MacOS/freespeak"
        )));
        assert!(!inside_app_bundle(Path::new("/Users/o/freespeak/target/release/freespeak")));
        // A directory that merely contains the letters must not count.
        assert!(!inside_app_bundle(Path::new("/Users/o/apps/freespeak")));
    }
}
