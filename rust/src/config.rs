//! Configuration: a tiny `key = value` file, environment overrides, and the
//! per-OS locations for config and logs.

use crate::tone::Tone;
use std::path::PathBuf;

/// Environment variables checked for the API key, in order.
const KEY_VARS: [&str; 3] = ["VOICE_NOT_API_KEY", "GROQ_API_KEY", "OPENAI_API_KEY"];

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    /// Base URL of an OpenAI-compatible API. The transcription endpoint is
    /// `{base_url}/audio/transcriptions`.
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub prompt: String,
    /// Whole-word `from -> to` corrections applied to every transcript.
    pub replacements: Vec<(String, String)>,
    pub device: String,
    pub hotkey: String,
    /// Stops the app. Empty disables it.
    pub quit_hotkey: String,
    /// `auto`, `winhttp` or `curl`.
    pub transport: String,
    pub paste: bool,
    pub beep: bool,
    /// How loud the tones are, 0.0-1.0.
    pub tone_volume: f32,
    /// Per-event sounds: keep the built-in note, silence it, or replace it.
    pub tone_start: Tone,
    pub tone_stop: Tone,
    pub tone_done: Tone,
    pub tone_error: Tone,
    pub save_transcripts: bool,
    pub paste_delay_ms: u64,
    pub min_ms: u64,
    pub silence_rms: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: String::new(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            // 216x realtime, and the cheaper of the two Whisper models on Groq.
            model: "whisper-large-v3-turbo".to_string(),
            language: String::new(),
            prompt: String::new(),
            replacements: Vec::new(),
            device: String::new(),
            hotkey: "ctrl+alt+space".to_string(),
            quit_hotkey: "ctrl+alt+shift+q".to_string(),
            transport: "auto".to_string(),
            paste: true,
            beep: true,
            tone_volume: crate::tone::DEFAULT_VOLUME,
            tone_start: Tone::Default,
            tone_stop: Tone::Default,
            tone_done: Tone::Default,
            tone_error: Tone::Default,
            save_transcripts: false,
            paste_delay_ms: 120,
            min_ms: 300,
            silence_rms: 0.003,
        }
    }
}

const TEMPLATE: &str = "\
# Voice Not configuration
# Everything here is optional except api_key.

# Your API key. The VOICE_NOT_API_KEY, GROQ_API_KEY and OPENAI_API_KEY
# environment variables all take precedence over this file.
api_key =

# Any OpenAI-compatible API works: the transcription endpoint is
# {base_url}/audio/transcriptions and the response needs a \"text\" field.
#   Groq        https://api.groq.com/openai/v1      whisper-large-v3-turbo
#   OpenAI      https://api.openai.com/v1           whisper-1, gpt-4o-transcribe
#   Mistral     https://api.mistral.ai/v1           voxtral-mini-latest
#   Together    https://api.together.xyz/v1         openai/whisper-large-v3
#   Fireworks   https://api.fireworks.ai/inference/v1
#   DeepInfra   https://api.deepinfra.com/v1/openai openai/whisper-large-v3
#   local       http://localhost:8080/v1            (plain http is fine)
base_url = https://api.groq.com/openai/v1

# Model name, as the provider spells it.
model = whisper-large-v3-turbo

# Language as an ISO-639-1 code: en, es, fr, de, it, pt, ja, ko...
#
# Setting this FORCES that language. It is not a hint: with `en` set, Italian
# speech comes back translated into English instead of transcribed, and with
# `it` set, English speech comes back in Italian. Empty means detect per
# recording, which measured the same speed and handled Italian, English, mixed
# and two-word utterances correctly.
language =

# Optional prompt, up to roughly 224 tokens, sent as Whisper's prompt field.
#   prompt = Technical dictation about databases. Always punctuate properly.
#
# Two cautions, both measured while building this:
#   * A bare comma-separated word list tends to strip punctuation and casing.
#   * It does not reliably fix homophones of unusual words (Groq -> Grok).
# For a deterministic fix use `replacements` below instead.
prompt =

# Literal word corrections applied to the transcript before it is pasted, as
# comma-separated `from=to` pairs. Matching is whole-word, and case-insensitive
# for ASCII (accented letters must match exactly); the replacement is inserted
# exactly as written. This is the reliable way to fix proper nouns and acronyms
# Whisper mishears.
#   replacements = Grok=Groq, postgres=Postgres, k8s=Kubernetes
replacements =

# Global hotkey. Modifiers: ctrl, alt, shift, win/cmd. Keys: a-z, 0-9, space,
# tab, enter, esc, f1-f12. On macOS, ctrl means the control key, not command, and
# macOS insists on at least one of ctrl, alt or cmd (shift alone is refused).
hotkey = ctrl+alt+space

# Hotkey that stops the app. It runs with no window, so this is the easy way
# out; set it empty to disable it (Task Manager can always end the process).
quit_hotkey = ctrl+alt+shift+q

# Input device to use, matched case-insensitively as a substring of the device
# name. Empty means the system default input device.
# Run `voice-not --devices` to list the names.
device =

# How to reach the API: auto, winhttp or curl.
#   auto     WinHTTP on Windows, curl on macOS/Linux
#   curl     always shell out to curl - useful behind odd corporate proxies
#   winhttp  Windows only
transport = auto

# Paste the transcript into whichever app has focus (Ctrl+V, or Cmd+V on macOS,
# is simulated). false = only put it on the clipboard.
paste = true

# Short tones on start / stop / success / error.
beep = true

# How loud those tones are, 0.0 to 1.0. They are deliberately quiet already;
# raise this if you dictate over loud music and cannot hear the cues.
tone_volume = 0.22

# The four sounds, one line each. Three ways to write each one:
#   tone_stop =                  built-in note (default, shown below)
#   tone_stop = off              no sound for that event at all
#   tone_stop = 660:55,494:95    your own notes, as frequency-in-Hz:length-in-ms
# Frequencies 20-20000 Hz, lengths 5-2000 ms, played in order.
#
#   tone_start  880:60     short high blip  - recording started
#   tone_stop   523:60     short low note   - recording stopped, clip sent
#   tone_done   1046:35    soft blip        - transcript pasted
#   tone_error  311:180    low buzz         - nothing was sent, or it failed
tone_start =
tone_stop =
tone_done =
tone_error =

# Append every transcript to transcripts.log next to this file.
save_transcripts = false

# Milliseconds to wait between putting text on the clipboard and pressing the
# paste key. Raise it if pasting into a heavy app drops the first characters.
paste_delay_ms = 120

# Ignore recordings shorter than this.
min_ms = 300

# Recordings quieter than this RMS level are treated as silence and are not
# sent to the API. Raise it if background noise triggers empty requests, lower
# it if your microphone is quiet and speech gets dropped.
silence_rms = 0.003
";

#[cfg(windows)]
fn default_data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("voice-not")
}

#[cfg(target_os = "macos")]
fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("voice-not")
}

#[cfg(not(any(windows, target_os = "macos")))]
fn default_data_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string())).join(".config")
        });
    base.join("voice-not")
}

pub fn data_dir() -> PathBuf {
    match std::env::var("VOICE_NOT_DATA_DIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => default_data_dir(),
    }
}

pub fn config_path() -> PathBuf {
    data_dir().join("config")
}

pub fn transcript_log_path() -> PathBuf {
    data_dir().join("transcripts.log")
}

pub fn log_path() -> PathBuf {
    data_dir().join("dictate.log")
}

fn parse_bool(v: &str, fallback: bool) -> bool {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => fallback,
    }
}

/// Cleans up a key that was pasted, typed, or read from a file.
///
/// Keys never contain whitespace, but they do pick up a UTF-8 BOM when read from
/// a file written by Windows PowerShell, and zero-width characters when copied
/// out of a web page. Either one rides along in the Authorization header and
/// makes every request fail authentication, so strip them here.
pub fn sanitize_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{feff}' && *c != '\u{200b}')
        .collect()
}

fn parse_replacements(value: &str) -> Vec<(String, String)> {
    value
        .split(',')
        .filter_map(|pair| {
            let (from, to) = pair.split_once('=')?;
            let from = from.trim();
            if from.is_empty() {
                return None;
            }
            Some((from.to_string(), to.trim().to_string()))
        })
        .collect()
}

impl Config {
    /// Loads the config file, creating a commented template on first run.
    pub fn load() -> Config {
        let mut cfg = Config::default();
        let path = config_path();

        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, TEMPLATE);
        }

        if let Ok(text) = std::fs::read_to_string(&path) {
            for raw in text.lines() {
                let line = raw.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                    continue;
                }
                let (key, value) = match line.split_once('=') {
                    Some(kv) => kv,
                    None => continue,
                };
                let key = key.trim().to_ascii_lowercase();
                let value = value.trim().trim_matches('"').trim();

                match key.as_str() {
                    "api_key" => cfg.api_key = sanitize_key(value),
                    "base_url" if !value.is_empty() => cfg.base_url = value.to_string(),
                    "hotkey" if !value.is_empty() => cfg.hotkey = value.to_string(),
                    "quit_hotkey" => cfg.quit_hotkey = value.to_string(),
                    "model" if !value.is_empty() => cfg.model = value.to_string(),
                    "language" => cfg.language = value.to_string(),
                    "prompt" => cfg.prompt = value.to_string(),
                    "device" => cfg.device = value.to_string(),
                    "transport" if !value.is_empty() => cfg.transport = value.to_string(),
                    "replacements" => cfg.replacements = parse_replacements(value),
                    "paste" => cfg.paste = parse_bool(value, cfg.paste),
                    "beep" => cfg.beep = parse_bool(value, cfg.beep),
                    "tone_volume" => {
                        if let Ok(volume) = value.parse::<f32>() {
                            cfg.tone_volume = volume.clamp(0.0, 1.0);
                        }
                    }
                    "tone_start" => cfg.tone_start = crate::tone::parse_tone(value),
                    "tone_stop" => cfg.tone_stop = crate::tone::parse_tone(value),
                    "tone_done" => cfg.tone_done = crate::tone::parse_tone(value),
                    "tone_error" => cfg.tone_error = crate::tone::parse_tone(value),
                    "save_transcripts" => {
                        cfg.save_transcripts = parse_bool(value, cfg.save_transcripts)
                    }
                    "paste_delay_ms" => {
                        if let Ok(n) = value.parse() {
                            cfg.paste_delay_ms = n;
                        }
                    }
                    "min_ms" => {
                        if let Ok(n) = value.parse() {
                            cfg.min_ms = n;
                        }
                    }
                    "silence_rms" => {
                        if let Ok(n) = value.parse() {
                            cfg.silence_rms = n;
                        }
                    }
                    _ => {}
                }
            }
        }

        // The environment always wins, so one-off runs never touch the file.
        for name in KEY_VARS {
            if let Ok(key) = std::env::var(name) {
                let key = sanitize_key(&key);
                if !key.is_empty() {
                    cfg.api_key = key;
                    break;
                }
            }
        }
        if let Ok(url) = std::env::var("VOICE_NOT_BASE_URL") {
            let url = url.trim();
            if !url.is_empty() {
                cfg.base_url = url.to_string();
            }
        }

        cfg
    }

    /// The full transcription URL, e.g. `https://api.groq.com/openai/v1/audio/transcriptions`.
    pub fn endpoint(&self) -> String {
        format!("{}/audio/transcriptions", self.base_url.trim_end_matches('/'))
    }

    pub fn transport(&self) -> crate::transport::Transport {
        crate::transport::Transport::parse(&self.transport)
    }

    pub fn append_transcript(&self, text: &str) {
        use std::io::Write;
        let path = transcript_log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let stamp = crate::platform::now_hms();
            let _ = writeln!(f, "[{}] {}", stamp, text);
        }
    }

    /// Applies the user's `from=to` corrections to a transcript.
    ///
    /// Words are split on anything that is not alphanumeric (apostrophes stay
    /// inside a word so "don't" is one token) and compared case-insensitively,
    /// so `grok=Groq` fixes "grok", "Grok" and "GROK" while leaving "grokish"
    /// alone. Everything that is not a replaced word is passed through verbatim,
    /// including punctuation and spacing.
    pub fn apply_replacements(&self, text: &str) -> String {
        if self.replacements.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut word = String::new();
        for ch in text.chars() {
            if ch.is_alphanumeric() || ch == '\'' {
                word.push(ch);
            } else {
                if !word.is_empty() {
                    out.push_str(&self.fix_word(&word));
                    word.clear();
                }
                out.push(ch);
            }
        }
        if !word.is_empty() {
            out.push_str(&self.fix_word(&word));
        }
        out
    }

    fn fix_word(&self, word: &str) -> String {
        for (from, to) in &self.replacements {
            if word.eq_ignore_ascii_case(from) {
                return to.clone();
            }
        }
        word.to_string()
    }
}

/// Writes the key into the config file, replacing the `api_key` line in place so
/// the surrounding comments survive. Creates the file from the template if it is
/// missing.
pub fn save_api_key(key: &str) -> Result<PathBuf, String> {
    let key = sanitize_key(key);
    if key.is_empty() {
        return Err("the key is empty".to_string());
    }
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| TEMPLATE.to_string());

    let mut out = String::with_capacity(existing.len() + key.len() + 16);
    let mut replaced = false;
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') && trimmed.starts_with("api_key") && trimmed.contains('=') {
            out.push_str(&format!("api_key = {key}\n"));
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&format!("api_key = {key}\n"));
    }

    std::fs::write(&path, out).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_replacements(pairs: &[(&str, &str)]) -> Config {
        Config {
            replacements: pairs
                .iter()
                .map(|(from, to)| (from.to_string(), to.to_string()))
                .collect(),
            ..Config::default()
        }
    }

    #[test]
    fn replacements_are_whole_word_and_case_insensitive() {
        let cfg = with_replacements(&[("grok", "Groq")]);
        assert_eq!(
            cfg.apply_replacements("Grok, grok and GROK went grokish."),
            "Groq, Groq and Groq went grokish."
        );
    }

    #[test]
    fn replacements_leave_the_rest_of_the_text_alone() {
        let cfg = with_replacements(&[("k8s", "Kubernetes")]);
        // Every occurrence is rewritten, and spacing/punctuation is untouched.
        assert_eq!(
            cfg.apply_replacements("Deploy k8s  now!\n(then k8s again)"),
            "Deploy Kubernetes  now!\n(then Kubernetes again)"
        );
        assert_eq!(cfg.apply_replacements("don't touch it"), "don't touch it");
        assert_eq!(cfg.apply_replacements("k8s."), "Kubernetes.");
    }

    #[test]
    fn parses_comma_separated_pairs() {
        assert_eq!(
            parse_replacements(" Grok = Groq , postgres=Postgres ,, bad "),
            vec![
                ("Grok".to_string(), "Groq".to_string()),
                ("postgres".to_string(), "Postgres".to_string()),
            ]
        );
        assert!(parse_replacements("").is_empty());
    }

    #[test]
    fn strips_hidden_characters_from_keys() {
        // A UTF-8 BOM (as written by Windows PowerShell) and a zero-width space.
        assert_eq!(sanitize_key("\u{feff}gsk_abc\u{200b}def\n"), "gsk_abcdef");
        assert_eq!(sanitize_key("  gsk_abc  "), "gsk_abc");
        assert_eq!(sanitize_key("\u{feff}   "), "");
    }

    #[test]
    fn endpoint_joins_base_url_and_path() {
        let cfg = Config {
            base_url: "https://api.openai.com/v1/".into(),
            ..Config::default()
        };
        assert_eq!(cfg.endpoint(), "https://api.openai.com/v1/audio/transcriptions");

        let local = Config {
            base_url: "http://localhost:8080/v1".into(),
            ..Config::default()
        };
        assert_eq!(local.endpoint(), "http://localhost:8080/v1/audio/transcriptions");
    }
}
