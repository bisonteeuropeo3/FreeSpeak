//! FreeSpeakSetup: the one-file Windows installer.
//!
//! `windows/tools/make-setup.ps1` appends the built `freespeak.exe` to this stub,
//! followed by its length and a magic marker, so nothing is embedded at compile
//! time and the stub can be built before the payload exists. Trailing bytes are
//! ignored by the PE loader, so the result is still a normal executable.
//!
//! Double-click  install, then offer to set the API key and start it
//! `--silent`    install with no dialogs (used by the automated tests)
//! `--uninstall` remove everything, including this file

#[cfg(not(windows))]
fn main() {
    eprintln!("FreeSpeakSetup only runs on Windows. On macOS use mac/build-mac.sh instead.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    installer::run();
}

#[cfg(windows)]
mod installer {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_YESNO,
    };

    const APP: &str = "FreeSpeak";
    const EXE: &str = "freespeak.exe";
    /// The image name the app had before it was renamed.
    const PREVIOUS_EXE: &str = "voice-not.exe";
    const UNINSTALLER: &str = "uninstall.exe";
    const MAGIC: &[u8; 8] = b"FSSETUP1";

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let silent = args
            .iter()
            .any(|a| a == "--silent" || a == "/S" || a == "/silent");

        // Second stage of an uninstall: this copy lives in %TEMP%, outside the
        // directory it is about to delete, so nothing it needs is locked.
        if let Some(index) = args.iter().position(|a| a == "--cleanup") {
            match args.get(index + 1) {
                Some(dir) => {
                    cleanup_dir(Path::new(dir));
                    return;
                }
                None => {
                    eprintln!("--cleanup needs a directory");
                    std::process::exit(2);
                }
            }
        }

        let uninstall_mode = args
            .iter()
            .any(|a| a == "--uninstall" || a == "/uninstall");

        let outcome = if uninstall_mode {
            uninstall(silent)
        } else {
            install(silent)
        };

        match outcome {
            Ok(()) => {
                if silent {
                    println!("ok");
                }
            }
            Err(err) => {
                if !silent {
                    info(&format!("{APP} could not be installed"), &err, true);
                }
                eprintln!("{err}");
                std::process::exit(1);
            }
        }
    }

    // ------------------------------------------------------------- locations

    pub fn install_dir() -> PathBuf {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("Programs").join("freespeak")
    }

    /// Where the app keeps its config. The data-directory variables are honoured
    /// here too: the installer has to agree with the app about where the key
    /// lives, or it would ask for a key that is already set.
    fn data_dir() -> PathBuf {
        for name in ["FREESPEAK_DATA_DIR", "VOICE_NOT_DATA_DIR"] {
            if let Ok(dir) = std::env::var(name) {
                if !dir.trim().is_empty() {
                    return PathBuf::from(dir);
                }
            }
        }
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("freespeak")
    }

    fn data_dir_is_forced() -> bool {
        ["FREESPEAK_DATA_DIR", "VOICE_NOT_DATA_DIR"]
            .iter()
            .any(|name| std::env::var(name).map(|v| !v.trim().is_empty()).unwrap_or(false))
    }

    fn config_path() -> PathBuf {
        data_dir().join("config")
    }

    /// The config file of the pre-rename app, if it is still there. The app moves
    /// it across on its first start; the installer has to look in both places or
    /// it would think the key was never set and ask for it again.
    fn previous_config_path() -> PathBuf {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("voice-not").join("config")
    }

    fn previous_install_dir() -> PathBuf {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("Programs").join("voice-not")
    }

    fn start_menu() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
    }

    // --------------------------------------------------------------- payload

    /// Reads the appended `freespeak.exe` out of this file.
    fn payload() -> Result<Vec<u8>, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("could not locate the installer: {e}"))?;
        let bytes = std::fs::read(&exe)
            .map_err(|e| format!("could not read {}: {e}", exe.display()))?;

        if bytes.len() < 32 || &bytes[bytes.len() - 8..] != MAGIC {
            return Err(
                "This file is not a FreeSpeak installer (no payload attached).\n\n\
                 Build one with windows\\build.ps1."
                    .to_string(),
            );
        }

        let mut length = [0u8; 8];
        length.copy_from_slice(&bytes[bytes.len() - 16..bytes.len() - 8]);
        let length = u64::from_le_bytes(length) as usize;

        let start = bytes
            .len()
            .checked_sub(16 + length)
            .ok_or_else(|| "the installer payload is truncated".to_string())?;
        Ok(bytes[start..start + length].to_vec())
    }

    fn has_api_key() -> bool {
        for name in [
            "FREESPEAK_API_KEY",
            "VOICE_NOT_API_KEY",
            "GROQ_API_KEY",
            "OPENAI_API_KEY",
        ] {
            if std::env::var(name).map(|v| !v.trim().is_empty()).unwrap_or(false) {
                return true;
            }
        }
        [config_path(), previous_config_path()]
            .iter()
            // A forced data directory means the old one is not this install's
            // business: the app will not migrate it either.
            .take(if data_dir_is_forced() { 1 } else { 2 })
            .any(|path| {
                std::fs::read_to_string(path)
                    .map(|text| {
                        text.lines().any(|line| {
                            let trimmed = line.trim_start();
                            !trimmed.starts_with('#')
                                && trimmed.starts_with("api_key")
                                && trimmed
                                    .split_once('=')
                                    .map(|(_, value)| !value.trim().is_empty())
                                    .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
    }

    // ------------------------------------------------------------ dialogs

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `FREESPEAK_NO_DIALOG=1` (or the pre-rename name) answers every dialog for
    /// the user: yes/no questions become "no", notices are skipped. That is what
    /// makes an interactive install testable from a script.
    fn dialogs_suppressed() -> bool {
        ["FREESPEAK_NO_DIALOG", "VOICE_NOT_NO_DIALOG"]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
    }

    fn ask(title: &str, text: &str) -> bool {
        if dialogs_suppressed() {
            return false;
        }
        let title = wide(title);
        let text = wide(text);
        let answer = unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONINFORMATION,
            )
        };
        answer == IDYES
    }

    fn info(title: &str, text: &str, error: bool) {
        if dialogs_suppressed() && !error {
            println!("{title}: {}", text.replace('\n', " "));
            return;
        }
        let title = wide(title);
        let text = wide(text);
        let style = if error {
            MB_OK | MB_ICONERROR
        } else {
            MB_OK | MB_ICONINFORMATION
        };
        unsafe {
            MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), style);
        }
    }

    // ------------------------------------------------------------ shortcuts

    /// Creation flags for helper processes: no console window may flash.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn hidden(mut command: Command) -> Command {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }

    fn quote(path: &Path) -> String {
        path.display().to_string().replace('\'', "''")
    }

    /// Start-menu shortcuts need IShellLink; WScript.Shell is by far the shortest
    /// route to one and PowerShell is always present.
    fn create_shortcut(
        link: &Path,
        target: &Path,
        arguments: &str,
        description: &str,
    ) -> Result<(), String> {
        if let Some(parent) = link.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let script = format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{link}'); \
             $s.TargetPath = '{target}'; $s.Arguments = '{arguments}'; \
             $s.Description = '{description}'; $s.IconLocation = '{target}'; $s.Save()",
            link = quote(link),
            target = quote(target),
            arguments = arguments.replace('\'', "''"),
            description = description.replace('\'', "''"),
        );
        let output = hidden(Command::new("powershell"))
            .args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
            .output()
            .map_err(|e| format!("could not run powershell: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(())
    }

    fn stop_running() {
        for image in [EXE, PREVIOUS_EXE] {
            let _ = Command::new("taskkill")
                .args(["/IM", image, "/F"])
                .output();
        }
    }

    /// Removes the copy installed under the app's previous name.
    ///
    /// Without this, upgrading left two apps installed side by side, two login
    /// entries competing, and two Start-menu entries - one of which still
    /// started the old build.
    fn remove_previous_install(silent: bool) {
        let dir = previous_install_dir();
        if !dir.exists() {
            return;
        }
        // Let the old build unregister itself first: it knows its own key name.
        let old_exe = dir.join(PREVIOUS_EXE);
        if old_exe.exists() {
            let _ = hidden(Command::new(&old_exe))
                .arg("--uninstall-autostart")
                .output();
        }

        let programs = start_menu();
        for name in ["Voice Not.lnk", "Uninstall Voice Not.lnk"] {
            let _ = std::fs::remove_file(programs.join(name));
        }

        // The old image may still be mapped for a moment after being killed.
        for attempt in 0..15 {
            if !dir.exists() {
                break;
            }
            let _ = std::fs::remove_dir_all(&dir);
            if !dir.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200 * (attempt.min(5) + 1)));
        }
        if !dir.exists() {
            if !silent {
                println!("removed the previous install {}", dir.display());
            }
        } else if !silent {
            println!(
                "note: {} could not be removed yet; delete it by hand once FreeSpeak is closed",
                dir.display()
            );
        }
    }

    /// Starts the app detached from this process.
    ///
    /// The stdio handles matter: without this the child inherits the installer's
    /// output, and anything that runs the installer through a pipe (`... | more`,
    /// a script capturing output, an IDE) then waits for the *child* to exit.
    /// Measured: a scripted install hung for five minutes on the settings window
    /// it had just opened.
    fn launch_detached(target: &Path, arguments: &[&str]) -> Result<(), String> {
        use std::process::Stdio;
        hidden(Command::new(target))
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not start {}: {e}", target.display()))
    }

    /// Puts `payload` in place of the installed app.
    ///
    /// Windows refuses to overwrite a running executable, and terminating it does
    /// not release the file the instant `taskkill` returns: the image stays
    /// mapped until the last handle to the dead process is closed, which can be
    /// a second or more when whatever started it still holds one. Installing over
    /// a running copy therefore failed outright - measured as `os error 32`,
    /// leaving the *old* version installed while reporting the reason on a
    /// console nobody reads. So: retry while the lock clears, and if it never
    /// does, rename the old file out of the way, which Windows does allow even
    /// while it runs.
    fn replace_app(target: &Path, payload: &[u8]) -> Result<(), String> {
        const ATTEMPTS: u32 = 15;
        for attempt in 0..ATTEMPTS {
            match std::fs::write(target, payload) {
                Ok(()) => return Ok(()),
                Err(_) if attempt + 1 < ATTEMPTS => {
                    std::thread::sleep(std::time::Duration::from_millis(200))
                }
                Err(_) => break,
            }
        }

        let old = target.with_extension("old.exe");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old).map_err(|e| {
            format!(
                "could not replace {}: {e}\n\nQuit FreeSpeak and run this installer again.",
                target.display()
            )
        })?;
        if let Err(err) = std::fs::write(target, payload) {
            // Do not leave the user with no app at all.
            let _ = std::fs::rename(&old, target);
            return Err(format!("could not write {}: {err}", target.display()));
        }
        // Still mapped, so this normally fails now; the next install or the
        // uninstaller clears it.
        let _ = std::fs::remove_file(&old);
        Ok(())
    }

    // -------------------------------------------------------------- install

    fn install(silent: bool) -> Result<(), String> {
        let payload = payload()?;
        let dir = install_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let target = dir.join(EXE);

        stop_running();
        // Left over from an upgrade where the running file had to be moved aside.
        let _ = std::fs::remove_file(target.with_extension("old.exe"));
        replace_app(&target, &payload)?;

        // Keep a copy of this installer around as the uninstaller.
        if let Ok(me) = std::env::current_exe() {
            let _ = std::fs::copy(&me, dir.join(UNINSTALLER));
        }

        // Autostart goes through the app's own tested code path.
        let _ = Command::new(&target).arg("--install-autostart").output();

        let programs = start_menu();
        // The Start-menu entry opens the settings, which is the only window this
        // app has; the background copy is started by the login entry.
        let _ = create_shortcut(
            &programs.join("FreeSpeak.lnk"),
            &target,
            "--settings",
            "FreeSpeak settings - API key and sound",
        );
        let _ = create_shortcut(
            &programs.join("Uninstall FreeSpeak.lnk"),
            &dir.join(UNINSTALLER),
            "--uninstall",
            "Remove FreeSpeak",
        );

        // Last, once this app is fully in place: drop the copy that was installed
        // under the previous name, its login entry and its Start-menu entries.
        remove_previous_install(silent);

        if silent {
            return Ok(());
        }

        if has_api_key() {
            if ask(
                APP,
                "FreeSpeak is installed and will start at login.\n\nStart it now?",
            ) {
                let _ = launch_detached(&target, &[]);
            }
        } else {
            // No key means the app cannot transcribe anything, and it used to
            // refuse to start at all - which looked like a broken install. Open
            // the settings window instead, so the one thing that is missing can
            // be filled in right there.
            info(
                APP,
                "FreeSpeak is installed and will start at login.\n\n\
                 It needs your API key before it can transcribe anything, so the \
                 settings window is opening now: paste the key and press Save.",
                false,
            );
            let _ = launch_detached(&target, &["--settings"]);
        }
        Ok(())
    }

    // ------------------------------------------------------------ uninstall

    fn uninstall(silent: bool) -> Result<(), String> {
        let dir = install_dir();
        let target = dir.join(EXE);

        if target.exists() {
            let _ = Command::new(&target).arg("--uninstall-autostart").output();
        }
        stop_running();
        // An upgrade may have left the pre-rename copy behind; uninstalling
        // should not leave half the app installed.
        remove_previous_install(true);

        let programs = start_menu();
        let _ = std::fs::remove_file(programs.join("FreeSpeak.lnk"));
        let _ = std::fs::remove_file(programs.join("Uninstall FreeSpeak.lnk"));

        // Remove everything except this running file.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(UNINSTALLER)
                {
                    continue;
                }
                let path = entry.path();
                let _ = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
            }
        }

        // The dialog, if any, must come before the hand-off: it keeps this
        // process alive, and the cleanup copy cannot delete a running
        // `uninstall.exe`. Spawning first would race the user's click.
        if !silent {
            info(
                APP,
                "FreeSpeak has been removed.\n\nYour config, API key and logs in \
                 %LOCALAPPDATA%\\freespeak were left alone.",
                false,
            );
        }

        // A running executable cannot delete itself, and Windows refuses to
        // remove a directory that still contains a running image. So hand the
        // last step to a copy of ourselves parked in %TEMP%, which is outside
        // everything it needs to remove.
        let me = std::env::current_exe().unwrap_or_default();
        let mut cleanup = std::env::temp_dir();
        cleanup.push(format!("freespeak-uninstall-{}.exe", std::process::id()));
        if std::fs::copy(&me, &cleanup).is_ok() {
            let _ = hidden(Command::new(&cleanup))
                .args(["--cleanup", &dir.display().to_string()])
                .spawn();
        }
        Ok(())
    }

    /// Runs from the %TEMP% copy: waits for the original uninstaller to exit,
    /// then removes the install directory for real.
    fn cleanup_dir(dir: &Path) {
        // The first stage exits within milliseconds of spawning us, but its
        // `uninstall.exe` stays undeletable until it does. Retry generously so a
        // slow antivirus scan or a lingering handle cannot strand the directory.
        for attempt in 0..40 {
            if !dir.exists() {
                return;
            }
            let _ = std::fs::remove_dir_all(dir);
            if !dir.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(150 * (attempt.min(6) + 1)));
        }

        // Out of retries: leave a note so the cause is not a mystery.
        let _ = std::fs::write(
            std::env::temp_dir().join("freespeak-uninstall.log"),
            format!(
                "could not remove {}\nstill present after 40 attempts\n",
                dir.display()
            ),
        );
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn replace_app_creates_the_app_and_overwrites_it_afterwards() {
            let dir = std::env::temp_dir().join(format!(
                "freespeak-setup-test-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let target = dir.join(EXE);
            let _ = std::fs::remove_file(&target);

            replace_app(&target, b"first build").unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"first build");

            // The upgrade case: the file is already there.
            replace_app(&target, b"second build").unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"second build");
            assert!(
                !target.with_extension("old.exe").exists(),
                "a clean overwrite must not leave the old copy behind"
            );

            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn the_payload_marker_is_not_confused_with_a_plain_build() {
            // Guards the check `payload()` relies on: a stub without an appended
            // payload must be rejected rather than installing garbage.
            let plain = std::env::current_exe().unwrap();
            let bytes = std::fs::read(&plain).unwrap();
            assert_ne!(
                &bytes[bytes.len() - 8..],
                MAGIC,
                "the test binary should have no payload attached"
            );
        }
    }
}
