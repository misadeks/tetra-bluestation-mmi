// Ringtone playback for incoming calls.
//
// This is fully self-contained and independent of the ACELP voice engine: it
// opens its own cpal output stream and SYNTHESIZES every ringtone (no audio
// files are shipped, so there are no assets or third-party samples to license).
// If no output device is available the player is simply absent and the UI runs
// unchanged (silent).
//
// The selected ringtone is a UI-only preference (see `prefs.rs`): it is stored
// locally and never sent to the stack.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};

/// The selectable ringtones: (id persisted in prefs, human label). The first is
/// the default.
pub const RINGTONES: &[(&str, &str)] = &[
    ("classic", "Classic"),
    ("warble", "Warble"),
    ("chirp", "Chirp"),
    ("pulse", "Pulse"),
    ("bell", "Bell"),
    ("digital", "Digital"),
];

/// The default ringtone id when none is stored.
pub fn default_id() -> &'static str {
    RINGTONES[0].0
}

/// True if `id` names a known ringtone.
pub fn is_valid(id: &str) -> bool {
    RINGTONES.iter().any(|(rid, _)| *rid == id)
}

struct Shared {
    /// The looping waveform (one full cadence period), mono at the device rate.
    buf: Mutex<Arc<Vec<i16>>>,
    /// Whether the stream should currently emit the waveform.
    active: AtomicBool,
    /// Read cursor into `buf`.
    pos: AtomicUsize,
    /// Master playback gain (0.0..1.0) as f32 bits.
    volume: AtomicU32,
    /// One-shot alert tone (plays `buf` once, then self-silences) vs a looping ring.
    oneshot: AtomicBool,
    /// Samples left to emit for the current one-shot tone.
    remaining: AtomicUsize,
}

pub struct RingtonePlayer {
    device: cpal::Device,
    config: cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    rate: u32,
    channels: usize,
    shared: Arc<Shared>,
    /// Id of the tone currently loaded, so repeated `play` calls don't restart it.
    current: Mutex<Option<String>>,
    /// The output stream is opened lazily while ringing and dropped when it stops.
    /// A continuously-running ALSA/I2S output stream contends with the co-located
    /// SDR's I2S DMA and blocks the radio, so we never hold it open while idle.
    stream: Mutex<Option<cpal::Stream>>,
}

impl RingtonePlayer {
    /// Probe the configured output device but DO NOT open a stream yet. Returns
    /// None if no device is available. The stream is created on demand in `play`.
    /// `output_device` is a cpal name substring (see `[audio].output_device`);
    /// "default"/empty uses the host default.
    pub fn new(output_device: &str) -> Option<RingtonePlayer> {
        let host = cpal::default_host();
        let dev = crate::audio::pick_output_device(&host, output_device)?;
        let cfg = dev.default_output_config().ok()?;
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels() as usize;
        let sample_format = cfg.sample_format();
        let config: cpal::StreamConfig = cfg.into();
        tracing::info!(rate, channels, "ringtone: ready (stream opens only while ringing)");
        Some(RingtonePlayer {
            device: dev,
            config,
            sample_format,
            rate,
            channels,
            shared: Arc::new(Shared {
                buf: Mutex::new(Arc::new(Vec::new())),
                active: AtomicBool::new(false),
                pos: AtomicUsize::new(0),
                volume: AtomicU32::new(1.0f32.to_bits()),
                oneshot: AtomicBool::new(false),
                remaining: AtomicUsize::new(0),
            }),
            current: Mutex::new(None),
            stream: Mutex::new(None),
        })
    }

    /// Build and start the output stream (called lazily from `play`).
    fn open_stream(&self) -> Option<cpal::Stream> {
        let channels = self.channels;
        let err = |e| tracing::warn!(error = %e, "ringtone: output stream error");
        macro_rules! out {
            ($t:ty, $conv:expr) => {{
                let shared = self.shared.clone();
                self.device
                    .build_output_stream(
                        &self.config,
                        move |data: &mut [$t], _| {
                            let active = shared.active.load(Ordering::Relaxed);
                            let gain = f32::from_bits(shared.volume.load(Ordering::Relaxed));
                            let oneshot = shared.oneshot.load(Ordering::Relaxed);
                            let wave = if active {
                                Some(shared.buf.lock().unwrap().clone())
                            } else {
                                None
                            };
                            let mut pos = shared.pos.load(Ordering::Relaxed);
                            let mut rem = shared.remaining.load(Ordering::Relaxed);
                            for frame in data.chunks_mut(channels) {
                                let s = match &wave {
                                    Some(w) if !w.is_empty() => {
                                        if oneshot {
                                            // Play the buffer once, then emit silence.
                                            if rem == 0 {
                                                0
                                            } else {
                                                let v = w[pos.min(w.len() - 1)];
                                                pos += 1;
                                                rem -= 1;
                                                v
                                            }
                                        } else {
                                            let v = w[pos % w.len()];
                                            pos = (pos + 1) % w.len();
                                            v
                                        }
                                    }
                                    _ => 0,
                                };
                                let s = (s as f32 * gain).clamp(-32768.0, 32767.0) as i16;
                                let v = $conv(s);
                                for slot in frame.iter_mut() {
                                    *slot = v;
                                }
                            }
                            shared.pos.store(pos, Ordering::Relaxed);
                            if oneshot {
                                shared.remaining.store(rem, Ordering::Relaxed);
                                if rem == 0 {
                                    // One-shot finished: go silent. stop() (called
                                    // from the app's ringtone sync) then closes the
                                    // stream on the next pass.
                                    shared.active.store(false, Ordering::Relaxed);
                                    shared.oneshot.store(false, Ordering::Relaxed);
                                }
                            }
                        },
                        err,
                        None,
                    )
                    .ok()
            }};
        }
        let stream = match self.sample_format {
            cpal::SampleFormat::F32 => out!(f32, |s: i16| s as f32 / 32768.0),
            cpal::SampleFormat::I16 => out!(i16, |s: i16| s),
            cpal::SampleFormat::U16 => out!(u16, |s: i16| (s as i32 + 32768) as u16),
            _ => {
                tracing::warn!("ringtone: unsupported output sample format");
                None
            }
        }?;
        stream.play().ok()?;
        Some(stream)
    }

    /// Start looping `id` (no-op if it is already playing). Unknown ids fall back
    /// to the default tone. Opens the output stream on first play.
    pub fn play(&self, id: &str) {
        let id = if is_valid(id) { id } else { default_id() };
        {
            let cur = self.current.lock().unwrap();
            if self.shared.active.load(Ordering::Relaxed) && cur.as_deref() == Some(id) {
                return; // already ringing with this tone
            }
        }
        let wave = Arc::new(synth(id, self.rate));
        *self.shared.buf.lock().unwrap() = wave;
        self.shared.pos.store(0, Ordering::Relaxed);
        self.shared.oneshot.store(false, Ordering::Relaxed);
        self.shared.active.store(true, Ordering::Relaxed);
        *self.current.lock().unwrap() = Some(id.to_string());
        // Open the stream lazily so it only exists (and contends with the radio's
        // I2S DMA) while a ringtone is actually sounding.
        let mut st = self.stream.lock().unwrap();
        if st.is_none() {
            *st = self.open_stream();
        }
    }

    /// Play a short one-shot alert tone once (e.g. talk-permit / clear-to-send).
    /// Skipped while a ringtone is sounding; the tone self-silences when done and
    /// the stream is closed by the next `stop()` from the ringtone sync.
    pub fn beep(&self, id: &str) {
        // Never talk over a ringtone.
        if self.shared.active.load(Ordering::Relaxed) {
            return;
        }
        let wave = synth_alert(id, self.rate);
        if wave.is_empty() {
            return;
        }
        let len = wave.len();
        *self.shared.buf.lock().unwrap() = Arc::new(wave);
        self.shared.pos.store(0, Ordering::Relaxed);
        self.shared.remaining.store(len, Ordering::Relaxed);
        self.shared.oneshot.store(true, Ordering::Relaxed);
        self.shared.active.store(true, Ordering::Relaxed);
        *self.current.lock().unwrap() = Some(id.to_string());
        let mut st = self.stream.lock().unwrap();
        if st.is_none() {
            *st = self.open_stream();
        }
    }

    /// Stop playback and close the output stream (silent + no DMA until next play).
    pub fn stop(&self) {
        // Do not cut off a one-shot alert tone that is still sounding; it silences
        // itself and a later stop() then closes the stream.
        if self.shared.oneshot.load(Ordering::Relaxed) && self.shared.active.load(Ordering::Relaxed)
        {
            return;
        }
        self.shared.active.store(false, Ordering::Relaxed);
        self.shared.oneshot.store(false, Ordering::Relaxed);
        *self.current.lock().unwrap() = None;
        // Drop the stream so it stops running and no longer contends with the SDR.
        *self.stream.lock().unwrap() = None;
    }

    /// Set the ringtone playback gain (0.0..1.0).
    pub fn set_volume(&self, v: f32) {
        self.shared.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

/// A tone segment: a set of simultaneous frequencies (empty = silence) for `ms`.
struct Seg {
    freqs: &'static [f32],
    ms: u32,
}

fn seg(freqs: &'static [f32], ms: u32) -> Seg {
    Seg { freqs, ms }
}

/// Synthesize one full cadence period of ringtone `id` as mono i16 at `rate`.
/// The buffer is looped by the stream, so the trailing silence is the gap
/// between rings.
fn synth(id: &str, rate: u32) -> Vec<i16> {
    // Peak amplitude (~ -12 dBFS) so the ring is audible but not harsh.
    const AMP: f32 = 8000.0;
    let segs: Vec<Seg> = match id {
        // Telephone-style double ring: two dual-tone bursts, then a long gap.
        "classic" => vec![
            seg(&[440.0, 480.0], 400),
            seg(&[], 200),
            seg(&[440.0, 480.0], 400),
            seg(&[], 1600),
        ],
        // Continuous warble alternating two close tones.
        "warble" => vec![
            seg(&[440.0], 150),
            seg(&[540.0], 150),
            seg(&[440.0], 150),
            seg(&[540.0], 150),
            seg(&[], 1200),
        ],
        // Two quick rising chirps.
        "chirp" => vec![
            seg(&[880.0], 90),
            seg(&[1320.0], 90),
            seg(&[], 120),
            seg(&[880.0], 90),
            seg(&[1320.0], 90),
            seg(&[], 1400),
        ],
        // Insistent triple beep.
        "pulse" => vec![
            seg(&[1000.0], 120),
            seg(&[], 90),
            seg(&[1000.0], 120),
            seg(&[], 90),
            seg(&[1000.0], 120),
            seg(&[], 1000),
        ],
        // Mellow decaying bell (handled specially below for the decay).
        "bell" => vec![seg(&[660.0, 990.0], 800), seg(&[], 1400)],
        // Ascending arpeggio.
        "digital" => vec![
            seg(&[660.0], 110),
            seg(&[880.0], 110),
            seg(&[1100.0], 110),
            seg(&[1320.0], 160),
            seg(&[], 1300),
        ],
        _ => vec![seg(&[440.0, 480.0], 400), seg(&[], 1600)],
    };
    let bell = id == "bell";

    let mut out: Vec<i16> = Vec::new();
    for s in &segs {
        let n = (rate as u64 * s.ms as u64 / 1000) as usize;
        // Short raised-cosine fades (5 ms) to avoid clicks at segment edges.
        let fade = ((rate as usize) * 5 / 1000).max(1);
        for i in 0..n {
            let t = i as f32 / rate as f32;
            let mut sample = 0.0f32;
            if !s.freqs.is_empty() {
                for &f in s.freqs {
                    sample += (2.0 * std::f32::consts::PI * f * t).sin();
                }
                sample /= s.freqs.len() as f32;
            }
            // Envelope: bell decays exponentially; others use edge fades.
            let env = if bell && !s.freqs.is_empty() {
                (-3.0 * (i as f32 / n.max(1) as f32)).exp()
            } else if i < fade {
                i as f32 / fade as f32
            } else if i + fade > n {
                (n - i) as f32 / fade as f32
            } else {
                1.0
            };
            out.push((sample * env * AMP) as i16);
        }
    }
    out
}

/// Alert-tone ids (short one-shot cues played via `beep`).
pub const TONE_TALK_PERMIT: &str = "talk-permit";
pub const TONE_CLEAR_TO_SEND: &str = "clear-to-send";

/// Synthesize a short one-shot alert tone (no trailing cadence gap).
fn synth_alert(id: &str, rate: u32) -> Vec<i16> {
    const AMP: f32 = 8000.0;
    // Talk-permit: a bright rising two-tone "go ahead". Clear-to-send: a single
    // softer mid beep signalling the channel is free to transmit.
    let segs: Vec<Seg> = match id {
        TONE_TALK_PERMIT => vec![seg(&[880.0], 45), seg(&[1174.7], 50)],
        TONE_CLEAR_TO_SEND => vec![seg(&[660.0], 140)],
        _ => return Vec::new(),
    };
    let mut out: Vec<i16> = Vec::new();
    for s in &segs {
        let n = (rate as u64 * s.ms as u64 / 1000) as usize;
        let fade = ((rate as usize) * 5 / 1000).max(1);
        for i in 0..n {
            let t = i as f32 / rate as f32;
            let mut sample = 0.0f32;
            if !s.freqs.is_empty() {
                for &f in s.freqs {
                    sample += (2.0 * std::f32::consts::PI * f * t).sin();
                }
                sample /= s.freqs.len() as f32;
            }
            let env = if i < fade {
                i as f32 / fade as f32
            } else if i + fade > n {
                (n - i) as f32 / fade as f32
            } else {
                1.0
            };
            out.push((sample * env * AMP) as i16);
        }
    }
    out
}
