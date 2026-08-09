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

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

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
}

pub struct RingtonePlayer {
    shared: Arc<Shared>,
    rate: u32,
    /// Id of the tone currently loaded, so repeated `play` calls don't restart it.
    current: Mutex<Option<String>>,
    // Keep the stream alive for the player's lifetime.
    _stream: cpal::Stream,
}

impl RingtonePlayer {
    /// Open the default output device and start a silent, ready ringtone stream.
    /// Returns None if no device is available.
    pub fn new() -> Option<RingtonePlayer> {
        let host = cpal::default_host();
        let dev = host.default_output_device()?;
        let cfg = dev.default_output_config().ok()?;
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels() as usize;
        let sample_format = cfg.sample_format();
        let config: cpal::StreamConfig = cfg.into();

        let shared = Arc::new(Shared {
            buf: Mutex::new(Arc::new(Vec::new())),
            active: AtomicBool::new(false),
            pos: AtomicUsize::new(0),
            volume: AtomicU32::new(1.0f32.to_bits()),
        });

        let err = |e| tracing::warn!(error = %e, "ringtone: output stream error");
        macro_rules! out {
            ($t:ty, $conv:expr) => {{
                let shared = shared.clone();
                dev.build_output_stream(
                    &config,
                    move |data: &mut [$t], _| {
                        let active = shared.active.load(Ordering::Relaxed);
                        let gain = f32::from_bits(shared.volume.load(Ordering::Relaxed));
                        // Snapshot the current waveform (cheap Arc clone).
                        let wave = if active {
                            Some(shared.buf.lock().unwrap().clone())
                        } else {
                            None
                        };
                        let mut pos = shared.pos.load(Ordering::Relaxed);
                        for frame in data.chunks_mut(channels) {
                            let s = match &wave {
                                Some(w) if !w.is_empty() => {
                                    let v = w[pos % w.len()];
                                    pos = (pos + 1) % w.len();
                                    v
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
                    },
                    err,
                    None,
                )
                .ok()
            }};
        }
        let stream = match sample_format {
            cpal::SampleFormat::F32 => out!(f32, |s: i16| s as f32 / 32768.0),
            cpal::SampleFormat::I16 => out!(i16, |s: i16| s),
            cpal::SampleFormat::U16 => out!(u16, |s: i16| (s as i32 + 32768) as u16),
            _ => {
                tracing::warn!("ringtone: unsupported output sample format");
                None
            }
        }?;
        stream.play().ok()?;
        tracing::info!(rate, channels, "ringtone: player ready");

        Some(RingtonePlayer {
            shared,
            rate,
            current: Mutex::new(None),
            _stream: stream,
        })
    }

    /// Start looping `id` (no-op if it is already playing). Unknown ids fall back
    /// to the default tone.
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
        self.shared.active.store(true, Ordering::Relaxed);
        *self.current.lock().unwrap() = Some(id.to_string());
    }

    /// Stop playback (silent until the next `play`).
    pub fn stop(&self) {
        self.shared.active.store(false, Ordering::Relaxed);
        *self.current.lock().unwrap() = None;
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
