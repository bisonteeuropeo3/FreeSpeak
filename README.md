# Voice Not

Press a global hotkey, talk, press it again, and the transcript lands in whatever
app you were typing in — transcribed by **any OpenAI-compatible API**.

It runs with **no window at all**: nothing in the taskbar, nothing in Alt-Tab,
nothing to close. It starts itself at login and waits quietly for the hotkey.

```
        hotkey                      mic                 {base_url}/audio/transcriptions
  you ──────────► record ──────────► mono 16 kHz WAV ──────────► transcript
                                                                     │
                       clipboard + paste keystroke  ◄───────────────┘
```

## Platform support

| | Windows 10/11 x64 | macOS | Linux |
| --- | --- | --- | --- |
| Capture, transcription, clipboard, tones | ✅ tested | ✅ built from the same portable code | ⚠️ portable layers exist, no hotkey backend yet |
| Global hotkeys | ✅ Win32 `RegisterHotKey` | ✅ Carbon `RegisterEventHotKey` (no permission needed) | ❌ not implemented |
| Microphone permission | ✅ granted at install | ⚠️ **required** since 10.14 — the `.app` carries `NSMicrophoneUsageDescription`; a bare binary cannot ask | ❌ |
| Paste keystroke | ✅ `SendInput` Ctrl+V | ✅ `CGEvent` Cmd+V (**needs Accessibility, added by hand**) | ❌ |
| Autostart at login | ✅ per-user Run key | ✅ LaunchAgent in `~/Library/LaunchAgents` | ❌ |
| Windowless | ✅ windowless subsystem | ✅ background-only process, no Dock icon | — |

> **Honesty note:** the macOS backend was written and reviewed on Windows, where
> it cannot be compiled or run — this machine's toolchain cannot even
> `cargo check` the macOS target (the CoreAudio bindings need `dlltool` with an
> assembler that is not installed). What *was* verified here: the portable layers
> it depends on (curl transport, WAV encoding, config, tones), a real
> transcription through the curl path, and the macOS-specific string logic
> (property list, AppleScript escaping, bundle detection), which now lives in
> shared code with tests that run on Windows. Treat the first macOS build as
> untested: run `mac/build-mac.sh` and report what breaks.

## Using it

1. Click into the app you want the text in — editor, chat box, email.
2. Press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Space</kbd>. A short high blip means
   it is recording; speak.
3. Press it again. **One short low note** confirms the click registered and the
   clip is on its way; a soft blip means the text has been pasted.

| Sound | Meaning |
| --- | --- |
| Short high blip (`880:60`) | Recording started |
| **One short low note** (`523:60`) | Recording stopped, transcribing now |
| Soft blip (`1046:35`) | Transcript pasted |
| Low buzz (`311:180`) | Nothing sent — silence, too short, or an API error (the log says which) |

The stop cue used to be two falling notes, which together with the pasted blip
read as three tones in a row; it is one short note now. Every cue can be retuned
or silenced in the config, by frequency in Hz and length in ms:

```ini
tone_volume = 0.22        ; 0.0 - 1.0, the whole set at once
tone_stop = 587:80        ; or tone_stop = off to drop that cue entirely
tone_done =               ; empty = keep the built-in note
```

The tones are real waveforms played through your default output device, so they
work the same on every platform (the old Windows `Beep` API could be silent on
some audio setups, which is why the stop sound was easy to miss).

**`voice-not --test-tones`** plays all four and reports how long each took to
reach the audio driver; `--test-tones 30` waits 30 seconds first, so the cue has
to wake a device that has gone to sleep. If a cue ever arrives late or not at
all, the log says so with numbers — playback waits for the driver to take every
sample instead of assuming a fixed delay, so a cue can be late but never cut
short.

It is a **toggle**, not push-to-talk: tap it, don't hold it. The transcript also
stays on your clipboard.

**To stop it:** press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>Q</kbd>,
or end the `voice-not` process in Task Manager / Activity Monitor. Starting it
twice is harmless: the second copy notices the first and exits.

## Any OpenAI-compatible provider

Yes — that was the point of the `base_url` setting. Voice Not sends a plain
OpenAI-style `multipart/form-data` POST to `{base_url}/audio/transcriptions`
with `file`, `model`, `language`, `prompt`, `response_format=json` and
`temperature`, and reads `text` out of the JSON reply. Anything implementing that
endpoint works:

```ini
# Groq (default) - cheapest and fastest
base_url = https://api.groq.com/openai/v1
model = whisper-large-v3-turbo

# OpenAI
base_url = https://api.openai.com/v1
model = gpt-4o-transcribe

# Mistral
base_url = https://api.mistral.ai/v1
model = voxtral-mini-latest

# Together / Fireworks / DeepInfra - same shape, different roots
base_url = https://api.together.xyz/v1
model = openai/whisper-large-v3

# A local server (Ollama, LM Studio, faster-whisper-server, whisper.cpp)
base_url = http://localhost:8080/v1
model = whisper-1
```

Plain `http://` is supported for local servers; `https://` is used everywhere
else. Each provider spells its model names differently, and prompts/language
support varies — if a provider ignores `prompt`, that is the provider, not a bug
here.

## Install

**Windows — double-click**

Run `windows\dist\VoiceNotSetup.exe`. That is the whole install: it copies the
app to `%LOCALAPPDATA%\Programs\voice-not`, registers the login entry, adds
**Voice Not** and **Uninstall Voice Not** to the Start menu, and then offers to
set your API key and start it.

To remove it, use **Uninstall Voice Not** in the Start menu, or run
`uninstall.exe --uninstall` from the install folder. Your config, key and logs
are always kept.

**Windows — from source**

```powershell
windows\build.ps1           # builds the app and packages the installer
windows\install.ps1         # install without going through the setup exe
windows\install.ps1 -Uninstall
```

**macOS**

```bash
mac/build-mac.sh                                            # needs Xcode CLT
open "mac/dist/Voice Not.app"                               # runs it, no window
"mac/dist/Voice Not.app/Contents/MacOS/voice-not" --set-key # paste your API key
"mac/dist/Voice Not.app/Contents/MacOS/voice-not" --install-autostart
```

The script produces a real `Voice Not.app` bundle, and that matters for two
reasons beyond tidiness:

* **Microphone.** Since macOS 10.14 the microphone is protected, and the usage
  string has to come from the bundle's `Info.plist`. A bare binary has no
  bundle, so macOS never even offers the permission and every recording comes
  back as silence — which looks exactly like a broken microphone.
* **Accessibility.** The grant is remembered per code signature. An unsigned
  binary in `target/release` changes identity on every `cargo build` and loses
  it silently; the bundle is signed ad hoc, so it keeps it.

Grant the microphone when macOS asks (or add the app by hand in System Settings →
Privacy & Security → **Microphone**). For pasting, add `Voice Not.app` with the
**+** button under **Accessibility** — Voice Not opens that pane for you when a
paste is blocked. Without it the transcript still reaches the clipboard, so
nothing is lost.

On the first run with no key, Voice Not asks for it and saves it. Later, change
it any time with `voice-not --set-key`. It also accepts `VOICE_NOT_API_KEY`,
`GROQ_API_KEY` or `OPENAI_API_KEY` from the environment, which take precedence.

Config and logs live in:

| | |
| --- | --- |
| Windows | `%LOCALAPPDATA%\voice-not\` |
| macOS | `~/Library/Application Support/voice-not/` |
| Linux | `~/.config/voice-not/` (or `$VOICE_NOT_DATA_DIR`) |

The important options, all documented in the generated config file:

```ini
api_key = gsk_...                 ; or use --set-key
base_url = https://api.groq.com/openai/v1
model = whisper-large-v3-turbo
language =                        ; empty = detect per recording (see below)
hotkey = ctrl+alt+space           ; on macOS, ctrl is control - write cmd+alt+space for Command
quit_hotkey = ctrl+alt+shift+q
replacements = VoiceNote=Voice Not, Grok=Groq
device =                          ; part of an input device name; see --devices
paste = true                      ; false = clipboard only
beep = true                       ; master switch for every cue
tone_volume = 0.22                ; how loud, 0.0 - 1.0
tone_stop =                       ; 523:60 by default, `off` to silence it
transport = auto                  ; auto, winhttp (Windows) or curl
```

### Command line

```
voice-not --set-key            ask for the API key and save it
voice-not --devices            list input devices
voice-not --init               create the config file
voice-not --install-autostart  start at login
voice-not --uninstall-autostart
voice-not --transcribe-file a.wav   transcribe a file and print it
voice-not --test-paste hello        check clipboard + paste keystroke
voice-not --test-tones [secs]       hear the four cues and time them
voice-not --hotkey ctrl+shift+d --language es --base-url http://localhost:8080/v1
```

On Windows, run these from a terminal: the release binary has no console of its
own, so it attaches to the terminal that launched it.

## Why it is light

| Choice | Why |
| --- | --- |
| Rust, `opt-level = "z"`, LTO, `panic = "abort"`, stripped | one 566 KB `.exe` on Windows, no runtime to install |
| No TLS crate | WinHTTP on Windows, the system `curl` elsewhere — no `rustls`, no `native-tls`, no OpenSSL |
| Windowless on Windows | Windows never creates a console, so there is no window to show or flash |
| Idle CPU is zero | no polling loop: a hotkey message loop, and the tone stream pauses itself when silent (the device is primed once at start-up so the first cue is not the slow one) |
| Capture stays on one thread | the audio callback is a preallocated `Vec<f32>` push, nothing on the hot path |
| 48 kHz → 16 kHz mono, box-filtered | 1/6th the upload of raw 48 kHz stereo; Whisper resamples to exactly that anyway |
| Transcription runs off-thread | you can start the next sentence while the previous one uploads |

Measured on Windows: a 566 KB `.exe`, 13.4 MB working set, 0 ms of CPU across a
5-second idle window, and a ~440 ms round trip for a short sentence.

## Troubleshooting

* **Is it running?** There is no window to look for. Task Manager / Activity
  Monitor shows the `voice-not` process, and the log tells the story.
* **A cue is late or missing** — run `voice-not --test-tones 30`. It plays each
  one, prints how long it took to reach the audio driver, and the log records
  any cue that had to wait. A cue is never cut short by a fixed delay any more;
  the earlier version guessed one and the start sound lost that race.
* **macOS: nothing is recorded, the log says "ignored: silence"** — the
  microphone permission is missing. Denied input returns silence rather than an
  error, so it looks like a mute microphone. Grant it in System Settings →
  Privacy & Security → Microphone, and make sure you are running the `.app`
  bundle: a bare binary has no `Info.plist` and can never be authorised.
* **macOS: nothing is pasted** — grant Accessibility (System Settings → Privacy &
  Security → Accessibility). Voice Not opens that pane for you the first time a
  paste is blocked. Hotkeys, recording and the tones need no permission.
* **Windows: the installer said the app was in use** — it now waits for the
  running copy to release the file and moves it aside if it will not, so
  upgrading over a running Voice Not works; if it still refuses, quit the app
  from Task Manager and run the installer again.
* **It translates instead of transcribing** — `language` is a *forcing* hint. With
  `language = en`, Italian speech comes back as English; with `it`, English comes
  back as Italian. Leave it empty to detect per recording, which measured the same
  speed and handled mixed and two-word utterances correctly.
* **A word is consistently misheard** — Whisper heard "Groq" as "Grok" and
  "Voice Not" as "VoiceNote" in testing. `prompt` is not a reliable fix (a
  comma-separated word list stripped punctuation; a full sentence still produced
  "Grog"). Use `replacements`: a deterministic whole-word rewrite before pasting.
* **HTTP 404 from your provider** — that provider has no `/audio/transcriptions`
  endpoint, or `base_url` is wrong. The error names the host it tried.
* **"could not register the hotkey"** — something else owns that combination
  (NVIDIA, PowerToys, Steam and ShareX all grab global keys). Pick another.
* **Nothing is pasted, but the transcript is in the log** — the focused window is
  running elevated (Windows) and blocks synthetic input. The text is on the
  clipboard, so paste it yourself.
* **"ignored: silence"** — below `silence_rms`; the log prints the measured level
  so you can calibrate it.
* **`VOICE_NOT_NO_DIALOG=1`** suppresses error dialogs, for scripted runs.

## Privacy

Audio goes to whichever provider `base_url` points at, under their terms. Nothing
else leaves the machine, and no audio files are written — recordings live in
memory and are dropped after the request. The API key is stored in plain text in
the config file, so treat that file like a credential.

## Layout

Three folders: the code, and one per platform.

```
rust/                  the whole program - this is what compiles for every OS
  src/main.rs            hotkey loop, start/stop state machine, paste orchestration
  src/capture.rs         WASAPI/CoreAudio capture, downmix, 48k->16k, WAV writing
  src/stt.rs             multipart request building, response parsing, provider errors
  src/transport/         winhttp.rs (Windows), curl.rs (macOS/Linux), URL parsing
  src/tone.rs            waveform feedback tones through the default output device
  src/platform/          mod.rs (shared parsing), windows.rs, macos.rs
  src/config.rs          config file, env overrides, key prompt/saving, corrections
  src/bin/setup.rs       the Windows installer stub
  target/                build output

windows/               everything specific to installing on Windows
  build.ps1              builds the app and packages the installer
  install.ps1            script install, without going through the setup exe
  dist/VoiceNotSetup.exe the double-click installer (generated)
  tools/make-icon.ps1    draws assets\voice-not.ico (no rc.exe on this toolchain)
  tools/embed-icon.ps1   injects icon + version info into a built exe
  tools/make-setup.ps1   appends the app to the setup stub
  assets/                the icon and its preview sheets

mac/                   the macOS wrapper
  build-mac.sh           builds via ../rust and assembles Voice Not.app
  dist/Voice Not.app     the bundle to double-click and to keep (generated)
```

The Rust crate is a single program for both operating systems; the platform split
lives in `src/platform/*.rs` behind `#[cfg]`, not in separate projects. The two
`dist/` folders are the deliverables: the Windows installer and the macOS bundle.
