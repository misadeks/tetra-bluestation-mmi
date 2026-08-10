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

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};

use crate::app::AppEvent;
use crate::config::AudioConfig;

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

/// The running voice engine. Owns the audio streams (kept alive) and shared
/// playback/uplink state. Created and dropped on the app loop thread.
pub struct AudioEngine {
    /// Raw downlink frames to the decoder thread (bounded, drop-oldest).
    dec_tx: Sender<(Vec<u8>, bool)>,
    dec_drop: Receiver<(Vec<u8>, bool)>,
    uplink_active: Arc<AtomicBool>,
    uplink_cid: Arc<AtomicU32>,
    /// Master playback gain (0.0..1.0) as f32 bits; read by the output callback.
    volume: Arc<AtomicU32>,
    _out_stream: cpal::Stream,
    _in_stream: cpal::Stream,
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
        let out_dev = host.default_output_device()?;
        let in_dev = host.default_input_device()?;
        let out_cfg = out_dev.default_output_config().ok()?;
        let in_cfg = in_dev.default_input_config().ok()?;
        let out_rate = out_cfg.sample_rate().0;
        let in_rate = in_cfg.sample_rate().0;
        let out_channels = out_cfg.channels() as usize;
        let in_channels = in_cfg.channels() as usize;
        tracing::info!(out_rate, in_rate, out_channels, in_channels, "audio: devices opened");

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
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));

        let out_stream = build_output_stream(
            &out_dev,
            &out_cfg,
            out_channels,
            playback.clone(),
            playing.clone(),
            prebuffer,
            volume.clone(),
        )?;
        out_stream.play().ok()?;

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
        // The capture->encode queue is bounded (drop-oldest, see build_input_stream)
        // so if the encoder ever runs below real time the uplink stays current
        // instead of accumulating unbounded latency (which the far end hears as a
        // fade to silence).
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

        let in_stream = build_input_stream(
            &in_dev,
            &in_cfg,
            in_channels,
            in_rate,
            uplink_active.clone(),
            enc_tx,
            enc_drop,
        )?;
        in_stream.play().ok()?;

        Some(AudioEngine {
            dec_tx,
            dec_drop,
            uplink_active,
            uplink_cid,
            volume,
            _out_stream: out_stream,
            _in_stream: in_stream,
        })
    }

    /// Set the master playback gain (0.0..1.0). Applied to downlink audio in the
    /// output callback. Lock-free: safe to call from the app-loop thread.
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
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
