//! Short feedback tones, rendered as waveforms and played through the default
//! output device.
//!
//! The Win32 `Beep` function used earlier is a single square-ish tone that many
//! audio setups route nowhere, which is why a stop sound could be silent. This
//! plays real samples through cpal, so it works the same on Windows, macOS and
//! Linux, and each event gets its own recognisable sound.
//!
//! # Why the start tone used to go missing
//!
//! An earlier version played a tone by resuming the stream, sleeping for the
//! tone's length plus 60 ms, and pausing again. It assumed the delay between
//! `play()` and the sound leaving the speakers was under 60 ms. Measured on this
//! machine, it is not: `--test-tones` reports the driver holding samples for
//! **63 ms** before they come out, and the audio callback taking another
//! **~44 ms** to pick them up. A 70 ms note therefore only became audible
//! ~107 ms after `play()`, while the window was 130 ms and `pause()` at 130 ms
//! cut it off mid-note: what was left was a click, which is exactly the "I only
//! sometimes hear the start sound" that was reported. Stop and Done survived
//! because their windows were longer and by then the device had just been awake.
//!
//! So playback no longer guesses. The audio callback counts the samples it has
//! actually handed to the driver, and `play` waits for that count to reach what
//! was queued before pausing, plus the device's own reported output latency so
//! the hardware buffer can drain. A tone can therefore be *late*, but never cut
//! short. [`Player::emit`] returns both numbers and the caller logs them, so the
//! log states plainly what the audio path did instead of hiding it.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Peak amplitude of the loudest tone, well below full scale so the tones are
/// not startling. Overridable with `tone_volume`.
///
/// The rendered samples already carry this (times a per-event gain), so the audio
/// callback does not scale them again: doing it in both places squared the
/// setting, which made `tone_volume = 0.22` come out at 0.048 and left no way to
/// reason about the number.
pub const DEFAULT_VOLUME: f32 = 0.08;

/// Longest we wait for the audio callback to take the samples before giving up
/// on the tone. Generous on purpose: a cold Windows endpoint can take most of a
/// second to wake, and taking 400 ms to beep beats not beeping at all.
const MAX_WAIT: Duration = Duration::from_millis(1500);

/// How long the start-up priming of the output device may take.
const PRIME_WAIT: Duration = Duration::from_secs(5);

/// Added to the measured output latency before pausing, so the samples already
/// sitting in the hardware buffer are heard rather than cut off.
const FLUSH_MARGIN: Duration = Duration::from_millis(40);
/// Bluetooth headsets report 150-300 ms of output latency, so the cap has to sit
/// above that or the tail of a tone gets cut off on exactly the slowest devices.
const MAX_FLUSH: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Recording started.
    Start,
    /// Recording stopped: the clip is on its way to the API.
    Stop,
    /// Transcript pasted.
    Done,
    /// Nothing was sent, or something failed.
    Error,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Start, Kind::Stop, Kind::Done, Kind::Error];
    pub const COUNT: usize = 4;

    fn index(self) -> usize {
        match self {
            Kind::Start => 0,
            Kind::Stop => 1,
            Kind::Done => 2,
            Kind::Error => 3,
        }
    }

    /// `(frequency in Hz, duration in ms)` pairs, played in order.
    ///
    /// Kept deliberately short: this fires next to speech, several times per
    /// dictation, so anything longer is noise rather than feedback.
    fn default_notes(self) -> &'static [(f32, f32)] {
        match self {
            Kind::Start => &[(880.0, 60.0)],
            // One short falling-free closing note. Two notes here read as
            // "three tones" together with the Done blip, and were reported as
            // too noisy.
            Kind::Stop => &[(523.0, 60.0)],
            Kind::Done => &[(1046.0, 35.0)],
            Kind::Error => &[(311.0, 180.0)],
        }
    }

    /// Per-event volume multiplier: the confirmations sit under the two events
    /// that carry information you must not miss.
    fn gain(self) -> f32 {
        match self {
            Kind::Start => 1.0,
            Kind::Stop => 0.85,
            Kind::Done => 0.7,
            Kind::Error => 1.0,
        }
    }
}

/// One event's sound, as written in the config file.
///
/// Three states, because "leave it alone" and "make it silent" have to be
/// distinguishable: `tone_stop =` keeps the built-in note, `tone_stop = off`
/// plays nothing, `tone_stop = 660:55,494:95` plays exactly those notes.
#[derive(Clone, Debug, PartialEq)]
pub enum Tone {
    Default,
    Silent,
    Notes(Vec<(f32, f32)>),
}

/// Reads `off`, an empty value, or `freq:ms[,freq:ms]` into a [`Tone`].
pub fn parse_tone(value: &str) -> Tone {
    let value = value.trim();
    if value.is_empty() {
        return Tone::Default;
    }
    match value.to_ascii_lowercase().as_str() {
        "off" | "none" | "silent" | "no" | "false" | "0" => Tone::Silent,
        "on" | "yes" | "true" | "1" => Tone::Default,
        _ => match parse_notes(value) {
            Some(notes) => Tone::Notes(notes),
            None => Tone::Default,
        },
    }
}

/// How the tones should sound, built from the config.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Master switch, from `beep = true/false` and `--no-beep`.
    pub enabled: bool,
    pub volume: f32,
    pub tones: [Tone; Kind::COUNT],
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            volume: DEFAULT_VOLUME,
            tones: std::array::from_fn(|_| Tone::Default),
        }
    }
}

impl Settings {
    /// The notes to play for an event, or `None` when that event is silent.
    pub fn notes(&self, kind: Kind) -> Option<&[(f32, f32)]> {
        match &self.tones[kind.index()] {
            Tone::Silent => None,
            Tone::Notes(notes) if !notes.is_empty() => Some(notes),
            _ => Some(kind.default_notes()),
        }
    }

    pub fn set(&mut self, kind: Kind, tone: Tone) {
        self.tones[kind.index()] = tone;
    }

    pub fn total_ms(&self, kind: Kind) -> f32 {
        self.notes(kind)
            .unwrap_or(&[])
            .iter()
            .map(|(_, ms)| ms)
            .sum()
    }

    fn amplitude(&self, kind: Kind) -> f32 {
        self.volume.clamp(0.0, 1.0) * kind.gain()
    }
}

/// Parses `880:60` or `660:55,494:95` into notes. Unparseable parts are skipped;
/// an empty or fully invalid value means "use the built-in tone".
pub fn parse_notes(value: &str) -> Option<Vec<(f32, f32)>> {
    let notes: Vec<(f32, f32)> = value
        .split(',')
        .filter_map(|note| {
            let (frequency, milliseconds) = note.split_once(':')?;
            let frequency: f32 = frequency.trim().parse().ok()?;
            let milliseconds: f32 = milliseconds.trim().parse().ok()?;
            // Keep this inside the audible range and short enough to stay
            // feedback rather than a jingle.
            if (20.0..=20_000.0).contains(&frequency) && (5.0..=2_000.0).contains(&milliseconds) {
                Some((frequency, milliseconds))
            } else {
                None
            }
        })
        .collect();
    if notes.is_empty() {
        None
    } else {
        Some(notes)
    }
}

/// Handle used by the rest of the program. Cheap to clone, safe to send.
#[derive(Clone)]
pub struct Tones {
    tx: Sender<Message>,
    settings: Arc<Mutex<Settings>>,
}

enum Message {
    Play(Kind),
    /// Ordering fence: answered once every earlier tone has finished playing.
    Barrier(Sender<()>),
    /// The settings changed; the next cue reads them again.
    Reload,
}

impl Tones {
    /// Starts the playback thread and opens the output device on it.
    ///
    /// The device is opened and primed immediately rather than on the first
    /// tone, so the (slow) one-off setup happens while the app is starting up
    /// instead of on the user's first hotkey press.
    pub fn start(settings: Settings) -> Tones {
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(settings));
        let worker_settings = shared.clone();
        std::thread::spawn(move || worker(rx, worker_settings));
        Tones { tx, settings: shared }
    }

    /// Applies settings that changed while the app was running - the settings
    /// window writes the config file, and the daemon hands the new values here -
    /// so silencing the cues takes effect on the very next keypress.
    ///
    /// Returns whether this flipped the sound on or off, which the caller logs:
    /// it is the one setting people change from the window, and "did it take
    /// effect?" should be answerable from the log rather than by pressing the
    /// hotkey and listening.
    pub fn set_settings(&self, settings: Settings) -> bool {
        let changed = self.current().enabled != settings.enabled;
        if let Ok(mut current) = self.settings.lock() {
            *current = settings;
        }
        let _ = self.tx.send(Message::Reload);
        changed
    }

    fn current(&self) -> Settings {
        self.settings
            .lock()
            .map(|settings| settings.clone())
            .unwrap_or_default()
    }

    /// Whether this event makes a sound at all.
    pub fn plays(&self, kind: Kind) -> bool {
        let settings = self.current();
        settings.enabled && settings.notes(kind).is_some()
    }

    /// Queues a tone. Never blocks: the caller is the dictation path.
    pub fn play(&self, kind: Kind) {
        if !self.plays(kind) {
            return;
        }
        let _ = self.tx.send(Message::Play(kind));
    }

    /// Waits until the playback thread has opened and primed the output device,
    /// and reports how long that took (`--test-tones`).
    pub fn ready(&self) -> Duration {
        let started = Instant::now();
        let (done_tx, done_rx) = channel();
        if self.tx.send(Message::Barrier(done_tx)).is_ok() {
            let _ = done_rx.recv_timeout(PRIME_WAIT);
        }
        started.elapsed()
    }

    /// Plays a tone and waits for it to finish. Only the `--test-tones`
    /// diagnostic uses this; dictation must never wait for a beep.
    /// `None` when the tone is switched off.
    pub fn play_and_wait(&self, kind: Kind) -> Option<Duration> {
        if !self.plays(kind) {
            return None;
        }
        let started = Instant::now();
        let _ = self.tx.send(Message::Play(kind));
        let (done_tx, done_rx) = channel();
        if self.tx.send(Message::Barrier(done_tx)).is_err() {
            return None;
        }
        let _ = done_rx.recv_timeout(MAX_WAIT + Duration::from_secs(2));
        Some(started.elapsed())
    }
}

/// Owns the output stream, so it lives on this thread (cpal streams are not
/// `Send` on every backend).
fn worker(rx: Receiver<Message>, settings: Arc<Mutex<Settings>>) {
    let shared = Arc::new(Shared::default());
    let player = match Player::new(shared) {
        Ok(player) => player,
        Err(err) => {
            crate::logging::line(&format!(
                "could not open an audio output for the tones ({err}); falling back to the system beep"
            ));
            while let Ok(message) = rx.recv() {
                match message {
                    Message::Play(kind) => fallback(kind),
                    Message::Barrier(done) => {
                        let _ = done.send(());
                    }
                    Message::Reload => {}
                }
            }
            return;
        }
    };

    // Prime the device now, so a cold endpoint is paid for at start-up.
    match player.prime() {
        Ok(elapsed) => crate::logging::line(&format!(
            "audio output ready in {:.0} ms ({} Hz, {} ch, {:.0} ms measured output latency)",
            elapsed.as_secs_f32() * 1000.0,
            player.sample_rate,
            player.channels,
            player.measured_latency().as_secs_f32() * 1000.0
        )),
        Err(err) => crate::logging::line(&format!("could not prime the audio output: {err}")),
    }

    while let Ok(message) = rx.recv() {
        match message {
            Message::Play(kind) => {
                // Read per cue, so a change made in the settings window applies
                // to the next keypress rather than the next restart.
                let settings = settings
                    .lock()
                    .map(|settings| settings.clone())
                    .unwrap_or_default();
                let samples = render(kind, &settings, player.sample_rate, player.channels);
                if samples.is_empty() {
                    continue; // this event is configured silent
                }
                let window = settings.total_ms(kind) + 60.0;
                match player.emit(samples) {
                    Ok(emit) => {
                        if emit.dropped {
                            crate::logging::line(&format!(
                                "{kind:?} tone was not taken by the audio driver within {} ms",
                                MAX_WAIT.as_millis()
                            ));
                        } else if emit.audible_at() > Duration::from_millis(window as u64) {
                            crate::logging::line(&format!(
                                "{kind:?} tone needed {:.0} ms to finish playing ({:.0} ms to hand over + {:.0} ms output latency): the old fixed {:.0} ms window would have cut it short",
                                emit.audible_at().as_secs_f32() * 1000.0,
                                emit.handed_over.as_secs_f32() * 1000.0,
                                emit.latency.as_secs_f32() * 1000.0,
                                window
                            ));
                        }
                    }
                    Err(err) => {
                        crate::logging::line(&format!("could not play the {kind:?} tone: {err}"))
                    }
                }
            }
            Message::Barrier(done) => {
                let _ = done.send(());
            }
            // Nothing to do: the next cue reads the new settings itself.
            Message::Reload => {}
        }
    }
}

#[derive(Default)]
struct Shared {
    queue: Mutex<VecDeque<f32>>,
    /// Samples actually popped by the audio callback and handed to the driver.
    handed_over: AtomicUsize,
    /// Highest output latency the driver has reported, in microseconds.
    latency_us: AtomicU32,
}

struct Player {
    stream: cpal::Stream,
    shared: Arc<Shared>,
    sample_rate: u32,
    channels: u16,
}

/// What a tone actually cost, for the log.
struct Emit {
    /// From "start playing" to "every sample handed to the driver".
    handed_over: Duration,
    /// How long the driver then holds those samples before they are heard.
    latency: Duration,
    /// True when the driver never took the samples and the tone was dropped.
    dropped: bool,
}

impl Emit {
    /// The earliest moment the whole tone can be out of the speakers.
    fn audible_at(&self) -> Duration {
        self.handed_over + self.latency
    }
}

impl Player {
    fn new(shared: Arc<Shared>) -> Result<Player, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("output config: {e}"))?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let channels = config.channels;
        let sample_rate = config.sample_rate.0;
        let on_error = |err: cpal::StreamError| {
            crate::logging::line(&format!("audio output error: {err}"));
        };

        macro_rules! build {
            ($sample:ty, $from:expr) => {{
                let shared = shared.clone();
                let convert = $from;
                device.build_output_stream(
                    &config,
                    move |data: &mut [$sample], info: &cpal::OutputCallbackInfo| {
                        // How far ahead of the speakers this callback is
                        // running: the samples written now are heard only after
                        // this delay, so it is also how long to wait before
                        // stopping the stream.
                        let stamp = info.timestamp();
                        if let Some(latency) = stamp.playback.duration_since(&stamp.callback) {
                            let micros = latency.as_micros().min(u32::MAX as u128) as u32;
                            shared.latency_us.fetch_max(micros, Ordering::Relaxed);
                        }

                        let mut handed_over = 0usize;
                        match shared.queue.lock() {
                            Ok(mut queue) => {
                                for sample in data.iter_mut() {
                                    match queue.pop_front() {
                                        // Already scaled by the renderer, which
                                        // knows this event's own gain.
                                        Some(value) => {
                                            *sample = convert(value);
                                            handed_over += 1;
                                        }
                                        None => *sample = convert(0.0),
                                    }
                                }
                            }
                            Err(_) => {
                                for sample in data.iter_mut() {
                                    *sample = convert(0.0);
                                }
                            }
                        }
                        if handed_over > 0 {
                            shared.handed_over.fetch_add(handed_over, Ordering::Relaxed);
                        }
                    },
                    on_error,
                    None,
                )
            }};
        }

        let stream = match sample_format {
            cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
            cpal::SampleFormat::I16 => build!(i16, |s: f32| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
            cpal::SampleFormat::U16 => {
                build!(u16, |s: f32| ((s.clamp(-1.0, 1.0) * 32767.0) + 32768.0) as u16)
            }
            other => return Err(format!("unsupported output sample format {other:?}")),
        }
        .map_err(|e| format!("opening the output device: {e}"))?;

        Ok(Player {
            stream,
            shared,
            sample_rate,
            channels,
        })
    }

    /// How long the driver reported holding samples, as measured by the audio
    /// callback. Zero until the first callback has run.
    fn measured_latency(&self) -> Duration {
        Duration::from_micros(self.shared.latency_us.load(Ordering::Relaxed) as u64)
    }

    /// Starts the stream and plays `samples`, then waits until the driver has
    /// taken all of them before pausing. Returns what the wait actually cost.
    fn emit(&self, samples: Vec<f32>) -> Result<Emit, String> {
        let total = samples.len();
        {
            let mut queue = self
                .shared
                .queue
                .lock()
                .map_err(|_| "the tone queue is poisoned".to_string())?;
            queue.clear();
            queue.extend(samples);
        }
        // Reset after the queue is filled: a callback still in flight from the
        // previous tone finds an empty queue and counts nothing.
        self.shared.handed_over.store(0, Ordering::Relaxed);

        let started = Instant::now();
        self.stream
            .play()
            .map_err(|e| format!("starting playback: {e}"))?;

        let deadline = started + MAX_WAIT;
        while self.shared.handed_over.load(Ordering::Relaxed) < total {
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let handed_over = started.elapsed();
        let dropped = self.shared.handed_over.load(Ordering::Relaxed) < total;
        let latency = Duration::from_micros(self.shared.latency_us.load(Ordering::Relaxed) as u64);

        // The driver holds the samples for its own buffer length before they
        // come out of the speakers; pausing now would cut the tail off.
        let flush = (latency + FLUSH_MARGIN).min(MAX_FLUSH);
        std::thread::sleep(flush);
        let _ = self.stream.pause();

        Ok(Emit {
            handed_over,
            latency,
            dropped,
        })
    }

    /// Opens, starts and stops the device once, with silence, so the first real
    /// tone does not pay for a suspended endpoint.
    fn prime(&self) -> Result<Duration, String> {
        let silence =
            vec![0.0f32; (self.sample_rate as usize / 100).max(1) * self.channels as usize];
        let started = Instant::now();
        let emit = self.emit(silence)?;
        if emit.dropped {
            return Err(format!(
                "the driver took no audio within {} ms",
                MAX_WAIT.as_millis()
            ));
        }
        Ok(started.elapsed())
    }
}

/// Sine notes with a short attack and release, so they do not click. An empty
/// result means this event is configured silent.
fn render(kind: Kind, settings: &Settings, sample_rate: u32, channels: u16) -> Vec<f32> {
    let notes = match settings.notes(kind) {
        Some(notes) => notes,
        None => return Vec::new(),
    };
    let channels = channels.max(1) as usize;
    let amplitude = settings.amplitude(kind);
    let mut out = Vec::new();
    for (frequency, milliseconds) in notes {
        let count = (sample_rate as f32 * milliseconds / 1000.0).max(1.0) as usize;
        let attack = (sample_rate as f32 * 0.005).max(1.0);
        let release = (sample_rate as f32 * 0.020).max(1.0);
        for index in 0..count {
            let position = index as f32;
            let envelope = if position < attack {
                position / attack
            } else if position > count as f32 - release {
                ((count as f32 - position) / release).max(0.0)
            } else {
                1.0
            };
            let time = position / sample_rate as f32;
            let value =
                (2.0 * std::f32::consts::PI * frequency * time).sin() * envelope * amplitude;
            for _ in 0..channels {
                out.push(value);
            }
        }
        let gap = (sample_rate as f32 * 0.015) as usize;
        for _ in 0..gap * channels {
            out.push(0.0);
        }
    }
    out
}

/// Used when no output device is available at all. Only Windows has a usable
/// fallback; elsewhere the tone is simply skipped.
#[cfg(windows)]
fn fallback(kind: Kind) {
    use windows::Win32::System::Diagnostics::Debug::Beep;
    let notes = kind.default_notes();
    let frequency = notes.first().map(|(hz, _)| *hz as u32).unwrap_or(880);
    let duration = notes.iter().map(|(_, ms)| *ms).sum::<f32>() as u32;
    unsafe {
        let _ = Beep(frequency, duration);
    }
}

#[cfg(not(windows))]
fn fallback(_kind: Kind) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(notes: Option<Vec<(f32, f32)>>) -> Settings {
        let mut settings = Settings::default();
        settings.set(
            Kind::Stop,
            match notes {
                Some(notes) => Tone::Notes(notes),
                None => Tone::Silent,
            },
        );
        settings
    }

    #[test]
    fn every_kind_renders_audio() {
        let settings = Settings::default();
        for kind in Kind::ALL {
            let samples = render(kind, &settings, 48_000, 2);
            assert!(!samples.is_empty(), "{kind:?} rendered nothing");
            // Interleaved stereo: the length must divide evenly by the channels.
            assert_eq!(samples.len() % 2, 0);
            let peak = samples.iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(peak > 0.05, "{kind:?} is inaudibly quiet (peak {peak})");
            assert!(peak <= 1.0, "{kind:?} would clip (peak {peak})");
        }
    }

    /// The reported complaint: stop + done sounded like three tones and made too
    /// much noise. Stop is now a single short note.
    #[test]
    fn the_closing_tone_is_one_short_note() {
        let settings = Settings::default();
        let notes = settings.notes(Kind::Stop).expect("the closing tone is on");
        assert_eq!(notes.len(), 1, "the closing tone must be a single note");
        assert!(notes[0].1 <= 80.0, "the closing note is too long: {notes:?}");
        assert!(settings.total_ms(Kind::Stop) < 100.0);
        // And the whole release sequence (stop + done) stays short.
        assert!(settings.total_ms(Kind::Stop) + settings.total_ms(Kind::Done) < 150.0);
    }

    #[test]
    fn tones_do_not_collide() {
        // Different frequency sets, so each event stays recognisable.
        let settings = Settings::default();
        for (index, kind) in Kind::ALL.iter().enumerate() {
            for other in Kind::ALL.iter().skip(index + 1) {
                let a: Vec<f32> = settings
                    .notes(*kind)
                    .unwrap()
                    .iter()
                    .map(|(hz, _)| *hz)
                    .collect();
                let b: Vec<f32> = settings
                    .notes(*other)
                    .unwrap()
                    .iter()
                    .map(|(hz, _)| *hz)
                    .collect();
                assert_ne!(a, b, "{kind:?} and {other:?} sound identical");
            }
        }
    }

    #[test]
    fn volume_scales_the_rendered_peak() {
        let quiet = Settings {
            volume: 0.05,
            ..Settings::default()
        };
        let peak = render(Kind::Start, &quiet, 48_000, 1)
            .iter()
            .fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(
            peak <= 0.05 + 1e-6,
            "tone_volume is not applied (peak {peak})"
        );
        assert!(peak > 0.0);
    }

    #[test]
    fn note_values_are_parsed_and_bad_ones_skipped() {
        assert_eq!(parse_notes("880:60"), Some(vec![(880.0, 60.0)]));
        assert_eq!(
            parse_notes(" 660:55 , 494 : 95 "),
            Some(vec![(660.0, 55.0), (494.0, 95.0)])
        );
        // Out of range, unparseable and empty values all fall back to the
        // built-in tone rather than silencing the event.
        assert_eq!(parse_notes("5:60"), None);
        assert_eq!(parse_notes("880:9000"), None);
        assert_eq!(parse_notes("880"), None);
        assert_eq!(parse_notes(""), None);
        assert_eq!(parse_notes("abc:def,880:60"), Some(vec![(880.0, 60.0)]));
    }

    #[test]
    fn a_tone_can_be_customised_or_turned_off() {
        assert_eq!(parse_tone(""), Tone::Default);
        assert_eq!(parse_tone("  "), Tone::Default);
        for off in ["off", "OFF", "none", "no", "false", "0", "silent"] {
            assert_eq!(parse_tone(off), Tone::Silent, "{off} should be silent");
        }
        assert_eq!(parse_tone("880:60"), Tone::Notes(vec![(880.0, 60.0)]));
        // Nonsense leaves the built-in sound in place, it does not mute it.
        assert_eq!(parse_tone("loud please"), Tone::Default);
    }

    #[test]
    fn a_configured_tone_replaces_the_default() {
        let settings = settings_with(Some(vec![(300.0, 30.0)]));
        assert_eq!(settings.notes(Kind::Stop), Some(&[(300.0, 30.0)][..]));
        let custom = render(Kind::Stop, &settings, 48_000, 1);
        let builtin = render(Kind::Stop, &Settings::default(), 48_000, 1);
        assert_ne!(custom.len(), builtin.len());
    }

    #[test]
    fn a_silenced_event_plays_nothing_and_a_broken_override_falls_back() {
        // `tone_stop = off`
        let silent = settings_with(None);
        assert_eq!(silent.notes(Kind::Stop), None);
        assert!(render(Kind::Stop, &silent, 48_000, 2).is_empty());
        assert!(!Tones::start(silent).plays(Kind::Stop));

        // An empty note list is not a configuration anyone can express by
        // accident, but if it happens the built-in note stays.
        let mut empty = Settings::default();
        empty.set(Kind::Stop, Tone::Notes(Vec::new()));
        assert_eq!(empty.notes(Kind::Stop), Some(Kind::Stop.default_notes()));
    }

    #[test]
    fn a_disabled_event_is_never_queued() {
        let mut settings = Settings::default();
        settings.set(Kind::Done, Tone::Silent);
        let tones = Tones::start(settings);
        assert!(tones.plays(Kind::Start));
        assert!(!tones.plays(Kind::Done));
        assert_eq!(tones.play_and_wait(Kind::Done), None);

        let off = Tones::start(Settings {
            enabled: false,
            ..Settings::default()
        });
        for kind in Kind::ALL {
            assert!(!off.plays(kind), "{kind:?} should be off with beep = false");
        }
    }
}
