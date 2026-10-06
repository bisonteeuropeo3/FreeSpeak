//! FreeSpeak: push-to-talk dictation for Windows, macOS and Linux.
//!
//! A global hotkey toggles microphone capture. On stop, the recording is sent to
//! an OpenAI-compatible transcription API, and the transcript is copied to the
//! clipboard and pasted into whichever window has focus.
//!
//! On Windows the release build is a windowless binary: Windows never creates a
//! console for it, so it cannot appear in the taskbar or in Alt-Tab. Started
//! from a terminal it attaches to that terminal instead, which is what keeps the
//! CLI flags below readable.

// No console window in release. Debug builds keep one while developing.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod capture;
mod config;
mod logging;
mod platform;
mod stt;
mod tone;
mod transport;

use config::Config;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tone::{Kind as ToneKind, Tones};

enum Command {
    Start,
    Stop,
}

fn main() {
    // A windowless Windows build has no console of its own; borrow the launching
    // terminal's, if there is one, so CLI output has somewhere to go.
    platform::bind_parent_console();

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    // Before anything reads or writes the config: move the settings across from
    // the folder this app used before it was renamed to FreeSpeak, so an
    // existing API key keeps working. One-shot, and skipped when there is
    // nothing to move.
    let migrated = config::migrate_legacy_data();
    logging::init(&config::log_path());
    if let Some(old) = migrated {
        logging::line(&format!(
            "moved your settings from {} to {} (the old folder can be deleted)",
            old.display(),
            config::data_dir().display()
        ));
    }

    if args.iter().any(|a| a == "--init") {
        let _ = Config::load(); // creates the commented template when missing
        out(&format!("config file: {}", config::config_path().display()));
        out("add your key with `freespeak --set-key`, or set FREESPEAK_API_KEY.");
        return;
    }

    if args.iter().any(|a| a == "--set-key") {
        set_key_interactively();
        return;
    }

    if args.iter().any(|a| a == "--devices") {
        match capture::list_devices() {
            Ok(names) => {
                out("Input devices:");
                for (index, name) in names.iter().enumerate() {
                    out(&format!("  [{index}] {name}"));
                }
                out("\nSet `device = <part of a name>` in the config file to pick one.");
            }
            Err(err) => out(&format!("could not list input devices: {err}")),
        }
        return;
    }

    if args.iter().any(|a| a == "--install-autostart") {
        match platform::install_autostart() {
            Ok(message) => out(&message),
            Err(err) => {
                out(&format!("could not register autostart: {err}"));
                std::process::exit(1);
            }
        }
        return;
    }

    if args.iter().any(|a| a == "--uninstall-autostart") {
        match platform::uninstall_autostart() {
            Ok(message) => out(&message),
            Err(err) => {
                out(&format!("could not remove autostart: {err}"));
                std::process::exit(1);
            }
        }
        return;
    }

    let mut cfg = Config::load();
    apply_args(&mut cfg, &args);

    // `--test-tones [seconds]`: play each cue so it can be heard, and report how
    // long it took to reach the audio driver. The optional number waits first,
    // which is how a suspended (cold) output device gets reproduced.
    if let Some(index) = args.iter().position(|a| a == "--test-tones") {
        let idle = args
            .get(index + 1)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        test_tones(&cfg, idle);
        return;
    }

    logging::line(&format!(
        "{} {} starting",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    ));
    logging::line(&format!("config: {}", config::config_path().display()));
    logging::line(&format!(
        "endpoint: {} ({:?} transport)",
        cfg.endpoint(),
        cfg.transport().resolve()
    ));

    // First run: ask for the key before anything else, so the very first hotkey
    // press works. Without a console there is nowhere to type it, so explain.
    if cfg.api_key.trim().is_empty() {
        if platform::has_console() {
            if !prompt_for_key() {
                std::process::exit(1);
            }
            cfg = Config::load();
        } else {
            fatal(&format!(
                "FreeSpeak has no API key yet.\n\nRun this in a terminal:\n    freespeak --set-key\n\nor put `api_key = ...` in\n{}",
                config::config_path().display()
            ));
        }
    }

    // Transcribe an existing audio file instead of the microphone. Handy for
    // checking the key and the endpoint without speaking.
    if let Some(path) = flag_value(&args, "--transcribe-file") {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                logging::line(&format!("could not read {path}: {err}"));
                std::process::exit(1);
            }
        };
        logging::line(&format!(
            "transcribing {path} ({:.0} kB)",
            bytes.len() as f64 / 1024.0
        ));
        match stt::transcribe(&cfg, &bytes) {
            Ok(text) => {
                let text = cfg.apply_replacements(&text);
                out(&text);
                if cfg.save_transcripts {
                    cfg.append_transcript(&text);
                }
            }
            Err(err) => {
                logging::line(&format!("transcription failed: {err}"));
                std::process::exit(1);
            }
        }
        return;
    }

    // Diagnostic for the two steps a hotkey test cannot reach without speaking:
    // putting text on the clipboard and sending the paste keystroke.
    if let Some(text) = flag_value(&args, "--test-paste") {
        logging::line(&format!("putting {text:?} on the clipboard"));
        if let Err(err) = platform::set_clipboard(&text) {
            logging::line(&format!("clipboard failed: {err}"));
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(150));
        match platform::send_paste() {
            Ok(()) => {
                logging::line("sent the paste keystroke to the focused window");
                logging::line("if the text did not appear, the window is elevated or not focused");
            }
            Err(err) => {
                logging::line(&format!("paste failed: {err}"));
                std::process::exit(1);
            }
        }
        return;
    }

    let toggle = match platform::parse_hotkey(&cfg.hotkey) {
        Ok(hotkey) => hotkey,
        Err(err) => fatal(&err),
    };

    // Only one copy can own the hotkeys, so a second launch (autostart plus a
    // manual start, say) exits quietly instead of failing to register them.
    let _instance = match platform::single_instance() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            logging::line("another instance is already running; exiting");
            return;
        }
        // Not the same thing as "already running": the lock file or mutex could
        // not be created at all, which used to be reported as a second instance.
        Err(err) => fatal(&format!(
            "{err}\n\nFreeSpeak needs to be able to write to {}",
            config::data_dir().display()
        )),
    };

    let tones = Tones::start(tone_settings(&cfg));

    // The microphone is owned by this thread for the life of the process:
    // cpal::Stream is not Send on every backend.
    let (command_tx, command_rx) = mpsc::channel::<Command>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
    let worker_cfg = cfg.clone();
    let worker_tones = tones.clone();
    std::thread::spawn(move || capture_worker(command_rx, ready_tx, worker_cfg, worker_tones));

    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(device)) => logging::line(&format!("microphone: {device}")),
        Ok(Err(err)) => fatal(&format!(
            "Could not open the microphone: {err}\n\nRun `freespeak --devices` in a terminal to list the inputs."
        )),
        Err(_) => fatal("Timed out opening the microphone."),
    }

    if let Err(err) = platform::register_hotkey(platform::HOTKEY_TOGGLE, &toggle) {
        // The platform layer knows why it failed, so its reason is reported
        // rather than a guess about another program owning the combination.
        fatal(&format!(
            "Could not register the hotkey '{}': {err}\n\nChange `hotkey` in the config file.",
            cfg.hotkey
        ));
    }

    // Optional quit hotkey: with no window there is otherwise no way out but the
    // task manager.
    let quit_registered = if cfg.quit_hotkey.trim().is_empty() {
        false
    } else {
        match platform::parse_hotkey(&cfg.quit_hotkey) {
            Ok(quit) => match platform::register_hotkey(platform::HOTKEY_QUIT, &quit) {
                Ok(()) => true,
                Err(err) => {
                    logging::line(&format!(
                        "could not register the quit hotkey '{}': {err}",
                        cfg.quit_hotkey
                    ));
                    false
                }
            },
            Err(err) => {
                logging::line(&format!("bad quit_hotkey: {err}"));
                false
            }
        }
    };

    logging::line(&format!(
        "ready - press {} to start and stop dictation.",
        cfg.hotkey
    ));
    if cfg.paste {
        logging::line("the transcript is pasted into the focused window.");
    } else {
        logging::line("the transcript is copied to the clipboard only (paste = false).");
    }
    if quit_registered {
        logging::line(&format!(
            "running in the background; {} quits.",
            cfg.quit_hotkey
        ));
    } else {
        logging::line("running in the background; end the freespeak process to quit.");
    }

    let mut recording = false;
    platform::run_message_loop(move |id| {
        if id == platform::HOTKEY_QUIT {
            logging::line("quit hotkey pressed; exiting");
            std::process::exit(0);
        }
        recording = !recording;
        let _ = command_tx.send(if recording {
            Command::Start
        } else {
            Command::Stop
        });
    });
}

/// Writes a line to stdout, tolerating a closed pipe instead of panicking the
/// way `println!` does.
fn out(text: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{text}");
}

/// Reports an error and stops. With no console there is nowhere to read it, so
/// this also raises a dialog.
fn fatal(message: &str) -> ! {
    logging::line(&format!("FATAL: {}", message.replace('\n', " ")));
    if !platform::has_console() {
        platform::alert(platform::APP_NAME, message);
    }
    std::process::exit(1);
}

/// Asks for the API key on the console and saves it into the config file.
fn prompt_for_key() -> bool {
    out("");
    out("FreeSpeak needs an API key for the transcription service.");
    out("  Groq    keys look like gsk_...  https://console.groq.com/keys");
    out("  OpenAI  keys look like sk-...   https://platform.openai.com/api-keys");
    out("");
    out(&format!(
        "Paste the key and press Enter (saved to {}):",
        config::config_path().display()
    ));

    let key = match platform::read_line() {
        Some(key) => key.trim().to_string(),
        None => {
            out("could not read a key from the terminal");
            return false;
        }
    };
    if key.is_empty() {
        out("no key entered");
        return false;
    }
    match config::save_api_key(&key) {
        Ok(path) => {
            out(&format!("saved to {}", path.display()));
            true
        }
        Err(err) => {
            out(&format!("could not save the key: {err}"));
            false
        }
    }
}

/// `--set-key`: prompt on the console, or fall back to opening the config file
/// when there is no console to type into.
fn set_key_interactively() {
    if prompt_for_key() {
        return;
    }
    if platform::has_console() {
        std::process::exit(1);
    }
    let _ = Config::load();
    let path = config::config_path();
    if let Err(err) = platform::open_in_editor(&path) {
        out(&format!("could not open {}: {err}", path.display()));
    }
    platform::alert(
        platform::APP_NAME,
        &format!(
            "Add a line like\n\n    api_key = gsk_...\n\nto\n{}\n\nand save the file.",
            path.display()
        ),
    );
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1).cloned()
}

fn apply_args(cfg: &mut Config, args: &[String]) {
    let mut index = 0;
    while index < args.len() {
        let value = args.get(index + 1).cloned();
        match args[index].as_str() {
            "--hotkey" => {
                if let Some(value) = value {
                    cfg.hotkey = value;
                    index += 1;
                }
            }
            "--device" => {
                if let Some(value) = value {
                    cfg.device = value;
                    index += 1;
                }
            }
            "--language" => {
                if let Some(value) = value {
                    cfg.language = value;
                    index += 1;
                }
            }
            "--base-url" => {
                if let Some(value) = value {
                    cfg.base_url = value;
                    index += 1;
                }
            }
            "--clipboard-only" => cfg.paste = false,
            "--no-beep" => cfg.beep = false,
            _ => {}
        }
        index += 1;
    }
}

/// Collects the tone configuration. The tone layer itself decides which events
/// make a sound, so the dictation path does not repeat those checks.
fn tone_settings(cfg: &Config) -> tone::Settings {
    let mut settings = tone::Settings {
        enabled: cfg.beep,
        volume: cfg.tone_volume,
        ..tone::Settings::default()
    };
    settings.set(ToneKind::Start, cfg.tone_start.clone());
    settings.set(ToneKind::Stop, cfg.tone_stop.clone());
    settings.set(ToneKind::Done, cfg.tone_done.clone());
    settings.set(ToneKind::Error, cfg.tone_error.clone());
    settings
}

/// `--test-tones`: play every cue and report its timing, so "I cannot hear the
/// start sound" can be answered with numbers instead of guesses.
fn test_tones(cfg: &Config, idle_seconds: u64) {
    let mut settings = tone_settings(cfg);
    // An explicit test plays even when `beep = false`; a deliberate per-event
    // `off` is still reported as off rather than overridden silently.
    settings.enabled = true;

    if idle_seconds > 0 {
        let message = format!("waiting {idle_seconds}s for the audio device to go idle...");
        out(&message);
        logging::line(&message);
        std::thread::sleep(Duration::from_secs(idle_seconds));
    }

    logging::line(&format!(
        "tone test: volume {:.2}, beep = {}, {}",
        settings.volume,
        cfg.beep,
        if cfg.tone_stop == tone::Tone::Default {
            "built-in cues"
        } else {
            "configured cues"
        }
    ));
    out(&format!(
        "config: {}",
        config::config_path().display()
    ));

    let tones = Tones::start(settings.clone());
    let ready = tones.ready();
    let message = format!(
        "output device ready in {:.0} ms (this is the one-off cost paid at start-up)",
        ready.as_secs_f32() * 1000.0
    );
    out(&message);
    logging::line(&message);

    for kind in ToneKind::ALL {
        let notes = match settings.notes(kind) {
            Some(notes) => notes.to_vec(),
            None => {
                let message = format!("{kind:?}: off (configured silent)");
                out(&message);
                logging::line(&message);
                continue;
            }
        };
        let description: Vec<String> = notes
            .iter()
            .map(|(hz, ms)| format!("{hz:.0} Hz for {ms:.0} ms"))
            .collect();
        match tones.play_and_wait(kind) {
            Some(elapsed) => {
                let message = format!(
                    "{kind:?}: {} - heard in {:.0} ms (cue itself is {:.0} ms)",
                    description.join(" + "),
                    elapsed.as_secs_f32() * 1000.0,
                    settings.total_ms(kind)
                );
                out(&message);
                logging::line(&message);
            }
            None => {
                let message = format!("{kind:?}: could not play");
                out(&message);
                logging::line(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    out("done - if you heard all of the cues above, the audio path is healthy.");
}

/// Owns the microphone for the life of the process and reacts to hotkey toggles.
fn capture_worker(
    commands: mpsc::Receiver<Command>,
    ready: mpsc::Sender<Result<String, String>>,
    cfg: Config,
    tones: Tones,
) {
    let microphone = match capture::Capture::new(&cfg.device) {
        Ok(microphone) => {
            let _ = ready.send(Ok(microphone.device_name().to_string()));
            microphone
        }
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };

    for command in commands {
        match command {
            Command::Start => {
                if let Err(err) = microphone.start() {
                    logging::line(&format!("could not start recording: {err}"));
                    tones.play(ToneKind::Error);
                    continue;
                }
                tones.play(ToneKind::Start);
                logging::line("recording...");
            }
            Command::Stop => {
                let samples = microphone.stop();
                // The stop tone fires straight away: it is the confirmation that
                // the click registered and the clip is on its way.
                tones.play(ToneKind::Stop);

                let seconds = samples.len() as f64 / microphone.sample_rate() as f64;
                if seconds * 1000.0 < cfg.min_ms as f64 {
                    logging::line(&format!("ignored: {seconds:.2}s is under min_ms"));
                    continue;
                }

                let level = capture::rms(&samples);
                if level < cfg.silence_rms {
                    logging::line(&format!("ignored: silence (rms {level:.4})"));
                    tones.play(ToneKind::Error);
                    continue;
                }

                let wav = capture::wav_from_mono(&samples, microphone.sample_rate());
                logging::line(&format!(
                    "sending {seconds:.2}s of audio ({:.0} kB)",
                    wav.len() as f64 / 1024.0
                ));

                // Transcribe off-thread so the next recording can start right away.
                let request_cfg = cfg.clone();
                let request_tones = tones.clone();
                std::thread::spawn(move || finish(&request_cfg, wav, &request_tones));
            }
        }
    }
}

fn finish(cfg: &Config, wav: Vec<u8>, tones: &Tones) {
    let started = Instant::now();
    let text = match stt::transcribe(cfg, &wav) {
        Ok(text) => cfg.apply_replacements(text.trim()),
        Err(err) => {
            logging::line(&format!("transcription failed: {err}"));
            tones.play(ToneKind::Error);
            return;
        }
    };

    if text.is_empty() {
        logging::line("no speech recognized");
        tones.play(ToneKind::Error);
        return;
    }

    logging::line(&format!(
        "transcript in {:.2}s: {text}",
        started.elapsed().as_secs_f32()
    ));
    if cfg.save_transcripts {
        cfg.append_transcript(&text);
    }

    if let Err(err) = platform::set_clipboard(&text) {
        logging::line(&format!("clipboard failed: {err}"));
        tones.play(ToneKind::Error);
        return;
    }

    if !cfg.paste {
        tones.play(ToneKind::Done);
        return;
    }

    std::thread::sleep(Duration::from_millis(cfg.paste_delay_ms));
    match platform::send_paste() {
        Ok(()) => {
            tones.play(ToneKind::Done);
        }
        Err(err) => {
            logging::line(&format!("paste failed: {err}"));
            logging::line("the transcript is on the clipboard - paste it yourself.");
        }
    }
}

fn print_help() {
    out(
        "FreeSpeak - push-to-talk dictation, transcribed by any OpenAI-compatible API

USAGE:
    freespeak [OPTIONS]

Press the hotkey (default ctrl+alt+space) to start recording, press it again to
stop. The transcript is copied to the clipboard and pasted into the focused
window. Cues: a short high blip = recording, a short low note = got it, a soft
blip = pasted, a low buzz = nothing was sent. Each one can be retuned or
silenced in the config (tone_start, tone_stop, tone_done, tone_error,
tone_volume).

There is no window at all: the quit hotkey (default ctrl+alt+shift+q) stops it,
or end the process in Task Manager / Activity Monitor.

OPTIONS:
    --set-key           ask for the API key and save it in the config
    --init              create the config file and exit
    --devices           list input devices and exit
    --hotkey <spec>     e.g. ctrl+alt+space, ctrl+shift+d
                        (macOS needs ctrl, alt or cmd in the combination)
    --device <text>     pick an input device by part of its name
    --language <code>   e.g. en, es, de (empty = detect automatically)
    --base-url <url>    any OpenAI-compatible API root
    --clipboard-only    copy to the clipboard without sending the paste key
    --no-beep           no cues at all
    --test-tones [secs] play the four cues and report how long each took to reach
                        the audio driver; [secs] waits first, to test a device
                        that has gone to sleep
    --transcribe-file <path>   transcribe a WAV/MP3/... and print it, then exit
    --test-paste <text>        clipboard + paste keystroke, to check they work
    --install-autostart        start at login
    --uninstall-autostart      stop starting at login
    -h, --help          show this help

The API key is read from FREESPEAK_API_KEY, GROQ_API_KEY or OPENAI_API_KEY, or
from `api_key = ...` in the config file. Run the tool once to create the config.

Config and logs live in:
    Windows  %LOCALAPPDATA%\\freespeak\\
    macOS    ~/Library/Application Support/freespeak/
    Linux    ~/.config/freespeak/"
    );
}
