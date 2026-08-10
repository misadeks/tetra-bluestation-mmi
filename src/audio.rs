// M5 two-way voice: ACELP decode (downlink) + encode (uplink) over cpal.
//
// Speech is coded with the bundled pure-Rust `tetra-acelp` crate (a Rust
// implementation of the ETSI EN 300 395-2 TETRA full-rate ACELP codec). The
// ETSI reference data tables it needs are NOT shipped here; they are generated
// once into the submodule via its `populate` tool (see README). If an audio
// device is missing, the engine is simply absent and the rest of the UI runs
// unchanged.
//
// Frame contract (mirrors the reference UI):
//   * One `MsSpeechFrame` carries 274 codec bits = 2 sub-frames of 137, which
//     decode to 480 int16 PCM samples (60 ms @ 8 kHz).
//   * Uplink: 480-sample (60 ms) frames encode to 274 bits shipped as
//     `MsUplinkSpeech`, one every 60 ms while we hold the floor.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};

use crate::app::AppEvent;
use crate::config::AudioConfig;

/// Print the cpal output/input device names, one per line, for `--list-audio`.
/// The printed name is exactly what goes in `[audio].output_device` /
/// `input_device` (a case-insensitive substring is enough).
pub fn list_devices() {
    let host = cpal::default_host();
    let def_out = host.default_output_device().and_then(|d| d.name().ok());
    let def_in = host.default_input_device().and_then(|d| d.name().ok());
    println!("Output devices ([audio].output_device):");
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            let name = d.name().unwrap_or_default();
            let star = if Some(&name) == def_out.as_ref() { " (default)" } else { "" };
            println!("  {name}{star}");
        }
    }
    println!("Input devices ([audio].input_device):");
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            let name = d.name().unwrap_or_default();
            let star = if Some(&name) == def_in.as_ref() { " (default)" } else { "" };
            println!("  {name}{star}");
        }
    }
}

/// Extract the `card=<name>` token from a cpal/ALSA name (lowercased), e.g.
/// "plughw:card=device,dev=0" -> "card=device".
fn card_token(name: &str) -> Option<String> {
    let start = name.find("card=")?;
    let rest = &name[start..];
    let end = rest.find(',').unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Choose a device: prefer an exact case-insensitive substring match of `want`;
/// otherwise fall back to any device on the same ALSA card (so hw:/plughw:/
/// default: variants of the same card match). Consumes the iterator.
fn choose_device(
    devs: impl Iterator<Item = cpal::Device>,
    want: &str,
) -> Option<cpal::Device> {
    let w = want.to_lowercase();
    let card = card_token(&w);
    let mut card_fallback: Option<cpal::Device> = None;
    for d in devs {
        let name = d.name().unwrap_or_default().to_lowercase();
        if name.contains(&w) {
            return Some(d); // exact substring wins
        }
        if card_fallback.is_none() {
            if let Some(c) = &card {
                if name.contains(c) {
                    card_fallback = Some(d);
                }
            }
        }
    }
    card_fallback
}

/// Pick an output device matching `want`. "default"/empty selects the host
/// default; falls back to it if nothing matches.
pub fn pick_output_device(host: &cpal::Host, want: &str) -> Option<cpal::Device> {
    if want.trim().is_empty() || want.eq_ignore_ascii_case("default") {
        return host.default_output_device();
    }
    let picked = host.output_devices().ok().and_then(|d| choose_device(d, want));
    match picked {
        Some(d) => {
            tracing::info!(device = %d.name().unwrap_or_default(), "audio: output device selected");
            Some(d)
        }
        None => {
            tracing::warn!(want, "audio: output device not found; using default");
            host.default_output_device()
        }
    }
}

/// Like [`pick_output_device`] for the capture side.
pub fn pick_input_device(host: &cpal::Host, want: &str) -> Option<cpal::Device> {
    if want.trim().is_empty() || want.eq_ignore_ascii_case("default") {
        return host.default_input_device();
    }
    let picked = host.input_devices().ok().and_then(|d| choose_device(d, want));
    match picked {
        Some(d) => {
            tracing::info!(device = %d.name().unwrap_or_default(), "audio: input device selected");
            Some(d)
        }
        None => {
            tracing::warn!(want, "audio: input device not found; using default");
            host.default_input_device()
        }
    }
}

const CODEC_RATE: u32 = 8000;
const SUBFRAME_SAMPLES: usize = 240;
const FRAME_SAMPLES: usize = 480; // 2 sub-frames, 60 ms
const SUBFRAME_BITS: usize = 137;
const FRAME_BITS: usize = 274;

/// A per-call decoder context wrapping the bundled Rust ACELP codec. Not Send:
/// construct and use it on a single thread (the decoder thread).
struct Decoder(tetra_acelp::Decoder);

impl Decoder {
    fn new() -> Decoder {
        Decoder(tetra_acelp::Decoder::new())
    }

    /// Decode a 274-bit speech frame into 480 PCM samples (concealing on `bad`).
    /// Bits are unpacked (one 0/1 byte each), two 137-bit sub-frames.
    fn decode(&mut self, bits: &[u8], bad: bool) -> Option<[i16; FRAME_SAMPLES]> {
        if bits.len() != FRAME_BITS {
            return None;
        }
        let mut pcm = [0i16; FRAME_SAMPLES];
        let quality = if bad {
            tetra_acelp::FrameQuality::bad()
        } else {
            tetra_acelp::FrameQuality::good()
        };
        for sf in 0..2 {
            let inp = &bits[sf * SUBFRAME_BITS..(sf + 1) * SUBFRAME_BITS];
            let mut b = [false; tetra_acelp::FRAME_BITS];
            for (i, &byte) in inp.iter().enumerate() {
                b[i] = byte != 0;
            }
            let frame = tetra_acelp::SpeechFrame::from_bits(&b);
            let out = self.0.decode(&frame, quality);
            pcm[sf * SUBFRAME_SAMPLES..(sf + 1) * SUBFRAME_SAMPLES].copy_from_slice(&out);
        }
        Some(pcm)
    }
}

/// A per-call encoder context wrapping the bundled Rust ACELP codec. Not Send:
/// construct and use it on a single thread (the encoder thread).
struct Encoder(tetra_acelp::Encoder);

impl Encoder {
    fn new() -> Encoder {
        Encoder(tetra_acelp::Encoder::new())
    }

    /// Encode 480 PCM samples into 274 unpacked codec bits (2 sub-frames).
    fn encode(&mut self, pcm: &[i16; FRAME_SAMPLES]) -> Option<Vec<u8>> {
        let mut bits = vec![0u8; FRAME_BITS];
        for sf in 0..2 {
            let mut pcm240 = [0i16; tetra_acelp::FRAME_SAMPLES];
            pcm240.copy_from_slice(&pcm[sf * SUBFRAME_SAMPLES..(sf + 1) * SUBFRAME_SAMPLES]);
            let frame = self.0.encode(&pcm240);
            let fb = frame.to_bits();
            let out = &mut bits[sf * SUBFRAME_BITS..(sf + 1) * SUBFRAME_BITS];
            for (i, &bit) in fb.iter().enumerate() {
                out[i] = bit as u8;
            }
        }
        Some(bits)
    }
}

/// Stateful linear resampler (mono). Cheap and good enough for 8 kHz voice.
struct Resampler {
    ratio: f64, // output samples per input sample
    pos: f64,
    last: f32,
    primed: bool,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Resampler {
        Resampler {
            ratio: to as f64 / from as f64,
            pos: 0.0,
            last: 0.0,
            primed: false,
        }
    }

    /// Push input samples, appending resampled output.
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &s in input {
            if !self.primed {
                self.last = s;
                self.primed = true;
            }
            // Emit output samples that fall between `last` and `s`.
            self.pos += self.ratio;
            while self.pos >= 1.0 {
                let frac = 1.0 - self.pos.fract();
                let v = self.last + (s - self.last) * frac as f32;
                out.push(v);
                self.pos -= 1.0;
            }
            self.last = s;
        }
    }
}

/// The running voice engine. Owns the audio stream *parameters* and shared
/// playback/uplink state. The cpal streams themselves are opened only while a
/// call is carrying media (see `set_active`) and dropped when idle: holding
/// them open continuously runs audio DMA that contends with the co-located
/// SX1255 I2S uplink and corrupts the timing-critical call-setup random access
/// (calls otherwise hang in "Connecting"/"Setting up"). Created, used, and
/// dropped on the app-loop thread only, so the stream cells are single-thread.
pub struct AudioEngine {
    /// Raw downlink frames to the decoder thread (bounded, drop-oldest).
    dec_tx: Sender<(Vec<u8>, bool)>,
    dec_drop: Receiver<(Vec<u8>, bool)>,
    uplink_active: Arc<AtomicBool>,
    uplink_cid: Arc<AtomicU32>,

    // Output (playback) stream parameters, kept so the stream can be (re)opened
    // on demand in `set_active`.
    out_dev: cpal::Device,
    out_cfg: cpal::SupportedStreamConfig,
    out_channels: usize,
    playback: Arc<Mutex<VecDeque<i16>>>,
    playing: Arc<AtomicBool>,
    prebuffer: usize,

    // Input (capture) stream parameters; None when no mic (downlink-only).
    in_parts: Option<InParts>,

    // Live cpal streams, present only while a call carries media. RefCell/Cell
    // because the engine is only ever touched from the app-loop thread.
    out_stream: RefCell<Option<cpal::Stream>>,
    in_stream: RefCell<Option<cpal::Stream>>,
    active: Cell<bool>,
    /// Master playback gain (0.0..1.0) as f32 bits; read by the output callback.
    volume: Arc<AtomicU32>,
}

/// Capture-side parameters retained to (re)build the input stream on demand.
struct InParts {
    dev: cpal::Device,
    cfg: cpal::SupportedStreamConfig,
    channels: usize,
    rate: u32,
    enc_tx: Sender<Vec<i16>>,
    enc_drop: Receiver<Vec<i16>>,
}

impl AudioEngine {
    /// Build the engine, or return None if disabled, the codec is unavailable,
    /// or audio devices can't be opened. Never panics.
    pub fn new(cfg: &AudioConfig, app_tx: Sender<AppEvent>) -> Option<AudioEngine> {
        if !cfg.enabled {
            tracing::info!("audio: disabled in config");
            return None;
        }
        tracing::info!("audio: ACELP codec = bundled tetra-acelp (Rust)");

        let host = cpal::default_host();
        let out_dev = pick_output_device(&host, &cfg.output_device)?;
        let out_cfg = out_dev.default_output_config().ok()?;
        let out_rate = out_cfg.sample_rate().0;
        let out_channels = out_cfg.channels() as usize;

        // The capture (mic) device is optional: without it we still decode and
        // play downlink voice, just with no uplink. This keeps voice audible even
        // when the input device can't be opened.
        let in_setup = pick_input_device(&host, &cfg.input_device)
            .and_then(|d| d.default_input_config().ok().map(|c| (d, c)));
        tracing::info!(
            out_rate,
            out_channels,
            has_mic = in_setup.is_some(),
            "audio: output opened"
        );

        // Jitter buffer: the stack delivers downlink frames in bursts with gaps
        // (esp. duplex, when both directions load the link), so hold ~jitter_ms of
        // decoded audio before starting playback and rebuffer on underrun. 0
        // disables it (immediate playback). Default is clamped to >= 1 frame.
        let prebuffer = if cfg.jitter_ms == 0 {
            0
        } else {
            (out_rate as usize * cfg.jitter_ms as usize / 1000).max(out_rate as usize * 60 / 1000)
        };
        tracing::info!(jitter_ms = cfg.jitter_ms, prebuffer_samples = prebuffer, "audio: jitter buffer");

        let playback: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
        let playing = Arc::new(AtomicBool::new(false));
        let uplink_active = Arc::new(AtomicBool::new(false));
        let uplink_cid = Arc::new(AtomicU32::new(0));

        // NOTE: the cpal output/input streams are intentionally NOT opened here.
        // An always-on stream runs continuous audio DMA for the whole app
        // lifetime, which contends with the co-located SX1255 I2S uplink and
        // corrupts the timing-critical call-setup random access (calls hang in
        // "Connecting"/"Setting up"). They are opened lazily in `set_active`
        // only while a call carries media, keeping the idle->setup window
        // contention-free. The decoder/encoder worker threads below are cheap
        // (they block on channels) and run for the whole session.

        // Decoder thread: raw downlink bits -> PCM -> playback queue. Runs OFF the
        // app-loop thread so heavy ACELP decode never delays the uplink relay. A
        // small bounded queue with drop-oldest keeps downlink latency in check.
        let (dec_tx, dec_rx) = crossbeam_channel::bounded::<(Vec<u8>, bool)>(8);
        let dec_drop = dec_rx.clone();
        {
            let playback = playback.clone();
            std::thread::Builder::new()
                .name("acelp-dec".into())
                .spawn(move || {
                    let mut dec = Decoder::new();
                    for (bits, bad) in dec_rx.iter() {
                        let Some(pcm8k) = dec.decode(&bits, bad) else { continue };
                        let mut rs = Resampler::new(CODEC_RATE, out_rate);
                        let input: Vec<f32> = pcm8k.iter().map(|&s| s as f32).collect();
                        let mut out =
                            Vec::with_capacity((FRAME_SAMPLES * out_rate as usize) / 8000 + 4);
                        rs.process(&input, &mut out);
                        let mut q = playback.lock().unwrap();
                        for v in out {
                            q.push_back(v.clamp(-32768.0, 32767.0) as i16);
                        }
                        // Bound latency: keep at most ~4x the jitter cushion (or
                        // ~1 s if none), dropping oldest if we get far ahead.
                        let cap = if prebuffer > 0 { prebuffer * 4 } else { out_rate as usize };
                        while q.len() > cap {
                            q.pop_front();
                        }
                    }
                })
                .ok()?;
        }

        // Encoder thread: PCM frames -> ACELP bits -> MsUplinkSpeech via app loop.
        // Uplink (capture -> ACELP encode -> MsUplinkSpeech) only when a mic is
        // available; otherwise the engine is downlink-only and still plays voice.
        // The capture stream itself is opened later in `set_active`; here we only
        // spawn the encoder thread and retain the parameters to build it.
        let in_parts = if let Some((in_dev, in_cfg)) = in_setup {
            let in_rate = in_cfg.sample_rate().0;
            let in_channels = in_cfg.channels() as usize;
            let (enc_tx, enc_rx) = crossbeam_channel::bounded::<Vec<i16>>(3);
            let enc_drop = enc_rx.clone();
            {
                let cid = uplink_cid.clone();
                std::thread::Builder::new()
                    .name("acelp-enc".into())
                    .spawn(move || {
                        let mut encoder = Encoder::new();
                        let mut sent: u64 = 0;
                        for frame in enc_rx.iter() {
                            if frame.len() != FRAME_SAMPLES {
                                continue;
                            }
                            let mut buf = [0i16; FRAME_SAMPLES];
                            buf.copy_from_slice(&frame);
                            if let Some(bits) = encoder.encode(&buf) {
                                let id = cid.load(Ordering::Relaxed);
                                if id != 0 {
                                    if sent % 50 == 0 {
                                        tracing::info!(cid = id, sent, "audio: uplink frames encoded/sent");
                                    }
                                    sent += 1;
                                    let _ = app_tx.send(AppEvent::UplinkAudio(id, bits));
                                }
                            }
                        }
                    })
                    .ok()?;
            }

            Some(InParts {
                dev: in_dev,
                cfg: in_cfg,
                channels: in_channels,
                rate: in_rate,
                enc_tx,
                enc_drop,
            })
        } else {
            tracing::warn!("audio: no capture device; downlink-only (uplink disabled)");
            None
        };

        Some(AudioEngine {
            dec_tx,
            dec_drop,
            uplink_active,
            uplink_cid,
            out_dev,
            out_cfg,
            out_channels,
            playback,
            playing,
            prebuffer,
            in_parts,
            out_stream: RefCell::new(None),
            in_stream: RefCell::new(None),
            active: Cell::new(false),
            volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        })
    }

    /// Set the master playback gain (0.0..1.0). Applied to downlink audio in the
    /// output callback. Lock-free: safe to call from the app-loop thread.
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Open the audio streams while a call carries media, or drop them when the
    /// call ends. Idempotent. Keeping streams closed between calls stops
    /// continuous audio DMA from starving the radio's I2S uplink during the
    /// timing-critical call-setup random access, which is what makes calls hang
    /// in "Connecting"/"Setting up". Only touched from the app-loop thread.
    pub fn set_active(&self, active: bool) {
        if active == self.active.get() {
            return;
        }
        if active {
            match build_output_stream(
                &self.out_dev,
                &self.out_cfg,
                self.out_channels,
                self.playback.clone(),
                self.playing.clone(),
                self.prebuffer,
                self.volume.clone(),
            ) {
                Some(s) if s.play().is_ok() => *self.out_stream.borrow_mut() = Some(s),
                _ => tracing::warn!("audio: failed to open output stream"),
            }
            if let Some(p) = &self.in_parts {
                match build_input_stream(
                    &p.dev,
                    &p.cfg,
                    p.channels,
                    p.rate,
                    self.uplink_active.clone(),
                    p.enc_tx.clone(),
                    p.enc_drop.clone(),
                ) {
                    Some(s) if s.play().is_ok() => *self.in_stream.borrow_mut() = Some(s),
                    _ => tracing::warn!("audio: failed to open input stream"),
                }
            }
            self.active.set(true);
            tracing::info!(has_mic = self.in_parts.is_some(), "audio: streams opened (call active)");
        } else {
            // Dropping the cpal streams closes the devices and stops all DMA.
            self.in_stream.borrow_mut().take();
            self.out_stream.borrow_mut().take();
            self.uplink_active.store(false, Ordering::Relaxed);
            self.playing.store(false, Ordering::Relaxed);
            if let Ok(mut q) = self.playback.lock() {
                q.clear();
            }
            self.active.set(false);
            tracing::info!("audio: streams closed (idle)");
        }
    }

    /// Queue a downlink speech frame for decoding on the decoder thread. Cheap:
    /// no decode happens on the caller (app-loop) thread. If the decoder has
    /// fallen behind, drop the oldest queued frame so latency stays bounded.
    pub fn play_downlink(&self, bits: &[u8], bad: bool) {
        let item = (bits.to_vec(), bad);
        match self.dec_tx.try_send(item) {
            Ok(()) => {}
            Err(crossbeam_channel::TrySendError::Full(item)) => {
                let _ = self.dec_drop.try_recv();
                let _ = self.dec_tx.try_send(item);
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
        }
    }

    /// Enable/disable uplink transmission for a call (floor-gated by the caller).
    pub fn set_uplink(&self, active: bool, cid: u32) {
        let new_cid = if active { cid } else { 0 };
        let prev = self.uplink_cid.swap(new_cid, Ordering::Relaxed);
        let was = self.uplink_active.swap(active, Ordering::Relaxed);
        if was != active || prev != new_cid {
            tracing::info!(active, cid = new_cid, "audio: uplink state changed");
        }
    }
}

fn build_output_stream(
    dev: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    channels: usize,
    playback: Arc<Mutex<VecDeque<i16>>>,
    playing: Arc<AtomicBool>,
    prebuffer: usize,
    volume: Arc<AtomicU32>,
) -> Option<cpal::Stream> {
    let sample_format = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.clone().into();
    let err = |e| tracing::warn!(error = %e, "audio: output stream error");
    macro_rules! out {
        ($t:ty, $conv:expr) => {{
            let playback = playback.clone();
            let playing = playing.clone();
            let volume = volume.clone();
            dev.build_output_stream(
                &config,
                move |data: &mut [$t], _| {
                    let mut q = playback.lock().unwrap();
                    let gain = f32::from_bits(volume.load(Ordering::Relaxed));
                    // Gate on the prebuffer: only start draining once enough audio
                    // has accumulated, and rebuffer after an underrun, so bursty
                    // delivery doesn't inject silence mid-word. prebuffer == 0
                    // means play immediately (buffer disabled).
                    let mut is_playing = playing.load(Ordering::Relaxed) || prebuffer == 0;
                    if !is_playing && q.len() >= prebuffer {
                        is_playing = true;
                    }
                    for frame in data.chunks_mut(channels) {
                        let s = if is_playing {
                            match q.pop_front() {
                                Some(v) => v,
                                None => {
                                    if prebuffer > 0 {
                                        is_playing = false;
                                    }
                                    0
                                }
                            }
                        } else {
                            0
                        };
                        // Apply master gain in the i16 domain (clamped).
                        let s = (s as f32 * gain).clamp(-32768.0, 32767.0) as i16;
                        let v = $conv(s);
                        for slot in frame.iter_mut() {
                            *slot = v;
                        }
                    }
                    playing.store(is_playing, Ordering::Relaxed);
                },
                err,
                None,
            )
            .ok()
        }};
    }
    match sample_format {
        cpal::SampleFormat::F32 => out!(f32, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::I16 => out!(i16, |s: i16| s),
        cpal::SampleFormat::U16 => out!(u16, |s: i16| (s as i32 + 32768) as u16),
        _ => {
            tracing::warn!("audio: unsupported output sample format");
            None
        }
    }
}

fn build_input_stream(
    dev: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    channels: usize,
    in_rate: u32,
    uplink_active: Arc<AtomicBool>,
    enc_tx: Sender<Vec<i16>>,
    enc_drop: Receiver<Vec<i16>>,
) -> Option<cpal::Stream> {
    let sample_format = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.clone().into();
    let err = |e| tracing::warn!(error = %e, "audio: input stream error");

    // Per-stream capture state (single audio thread owns it).
    let mut resampler = Resampler::new(in_rate, CODEC_RATE);
    let mut acc: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2);
    let mut mono: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();

    let mut feed = move |samples_f32: &[f32]| {
        if !uplink_active.load(Ordering::Relaxed) {
            // Not transmitting: keep the resampler primed but drop output.
            return;
        }
        mono.clear();
        for frame in samples_f32.chunks(channels) {
            let sum: f32 = frame.iter().copied().sum();
            mono.push(sum / channels as f32);
        }
        resampled.clear();
        resampler.process(&mono, &mut resampled);
        for &v in &resampled {
            acc.push((v * 32767.0).clamp(-32768.0, 32767.0) as i16);
        }
        while acc.len() >= FRAME_SAMPLES {
            let frame: Vec<i16> = acc.drain(0..FRAME_SAMPLES).collect();
            // Bounded, drop-oldest: if the encoder has fallen behind, discard the
            // oldest queued frame and enqueue the newest so uplink latency stays
            // capped instead of growing without bound.
            match enc_tx.try_send(frame) {
                Ok(()) => {}
                Err(crossbeam_channel::TrySendError::Full(f)) => {
                    let _ = enc_drop.try_recv();
                    let _ = enc_tx.try_send(f);
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
            }
        }
    };

    macro_rules! inp {
        ($t:ty, $conv:expr) => {{
            let mut scratch: Vec<f32> = Vec::new();
            dev.build_input_stream(
                &config,
                move |data: &[$t], _| {
                    scratch.clear();
                    scratch.extend(data.iter().map(|&s| $conv(s)));
                    feed(&scratch);
                },
                err,
                None,
            )
            .ok()
        }};
    }
    match sample_format {
        cpal::SampleFormat::F32 => inp!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => inp!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::U16 => inp!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        _ => {
            tracing::warn!("audio: unsupported input sample format");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Rust backend's unpacked-bit wire mapping must match the underlying
    /// `tetra-acelp` crate exactly (this is what keeps it interoperable with the
    /// ETSI path on the wire).
    #[test]
    fn rust_backend_bit_mapping_matches_crate() {
        let mut enc = Encoder::new();
        let mut dec = Decoder::new();

        // A non-trivial 480-sample (two sub-frame) input.
        let mut pcm = [0i16; FRAME_SAMPLES];
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = ((i as f32 * 0.11).sin() * 8000.0) as i16;
        }

        let bits = enc.encode(&pcm).expect("encode");
        assert_eq!(bits.len(), FRAME_BITS);
        assert!(bits.iter().all(|&b| b <= 1), "bits must be unpacked 0/1");

        // Cross-check every sub-frame's bits against the crate used directly.
        let mut cenc = tetra_acelp::Encoder::new();
        for sf in 0..2 {
            let mut p = [0i16; tetra_acelp::FRAME_SAMPLES];
            p.copy_from_slice(&pcm[sf * SUBFRAME_SAMPLES..(sf + 1) * SUBFRAME_SAMPLES]);
            let fb = cenc.encode(&p).to_bits();
            for i in 0..SUBFRAME_BITS {
                assert_eq!(bits[sf * SUBFRAME_BITS + i], fb[i] as u8, "bit {i} sf {sf}");
            }
        }

        // Decode returns a full 480-sample frame (good and concealed).
        assert_eq!(dec.decode(&bits, false).expect("decode").len(), FRAME_SAMPLES);
        assert_eq!(dec.decode(&bits, true).expect("conceal").len(), FRAME_SAMPLES);
    }
}
