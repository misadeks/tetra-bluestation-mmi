// M5 two-way voice: ACELP decode (downlink) + encode (uplink) over cpal.
//
// The ETSI EN 300 395-2 TETRA ACELP codec is copyrighted and never shipped in
// this repo. We load the prebuilt decoder/encoder shared libraries at runtime
// (path from `[audio].codec_dir`) via a tiny stable C ABI (see the reference
// `native/acelp_decode.c` / `acelp_encode.c`). If the libraries or an audio
// device are missing, the engine is simply absent and the rest of the UI runs
// unchanged.
//
// Frame contract (mirrors the reference UI):
//   * One `MsSpeechFrame` carries 274 codec bits = 2 sub-frames of 137, which
//     decode to 480 int16 PCM samples (60 ms @ 8 kHz).
//   * Uplink: 480-sample (60 ms) frames encode to 274 bits shipped as
//     `MsUplinkSpeech`, one every 60 ms while we hold the floor.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;
use libloading::Library;

use crate::app::AppEvent;
use crate::config::AudioConfig;

const CODEC_RATE: u32 = 8000;
const SUBFRAME_SAMPLES: usize = 240;
const FRAME_SAMPLES: usize = 480; // 2 sub-frames, 60 ms
const SUBFRAME_BITS: usize = 137;
const FRAME_BITS: usize = 274;

type DecCreate = unsafe extern "C" fn() -> *mut c_void;
type DecDestroy = unsafe extern "C" fn(*mut c_void);
type DecDecode = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut i16) -> i32;
type EncCreate = unsafe extern "C" fn() -> *mut c_void;
type EncDestroy = unsafe extern "C" fn(*mut c_void);
type EncEncode = unsafe extern "C" fn(*mut c_void, *const i16, *mut u8) -> i32;

/// Loaded codec libraries + resolved entry points. Shareable across threads;
/// each thread creates its own per-call decoder/encoder context.
struct CodecLib {
    _dec_lib: Library,
    _enc_lib: Library,
    dec_create: DecCreate,
    dec_destroy: DecDestroy,
    dec_decode: DecDecode,
    enc_create: EncCreate,
    enc_destroy: EncDestroy,
    enc_encode: EncEncode,
}

// The raw fn pointers reference code owned by the kept-alive Library handles.
unsafe impl Send for CodecLib {}
unsafe impl Sync for CodecLib {}

fn lib_name(stem: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("{stem}.dll")
    }
    #[cfg(target_os = "macos")]
    {
        format!("lib{stem}.dylib")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        format!("lib{stem}.so")
    }
}

impl CodecLib {
    fn load(dir: &Path) -> Option<CodecLib> {
        let dec_path = dir.join(lib_name("tetra_acelp"));
        let enc_path = dir.join(lib_name("tetra_acelp_enc"));
        // Build the libraries on demand from the ETSI sources when they are
        // missing (mirrors app/acelp.py in the Python repo).
        ensure_built(dir, &dec_path, &enc_path);
        unsafe {
            let dec_lib = Library::new(&dec_path)
                .map_err(|e| tracing::warn!(path = %dec_path.display(), error = %e, "codec: decoder load failed"))
                .ok()?;
            let enc_lib = Library::new(&enc_path)
                .map_err(|e| tracing::warn!(path = %enc_path.display(), error = %e, "codec: encoder load failed"))
                .ok()?;
            let dec_create = *dec_lib.get::<DecCreate>(b"tetra_dec_create").ok()?;
            let dec_destroy = *dec_lib.get::<DecDestroy>(b"tetra_dec_destroy").ok()?;
            let dec_decode = *dec_lib.get::<DecDecode>(b"tetra_dec_decode").ok()?;
            let enc_create = *enc_lib.get::<EncCreate>(b"tetra_enc_create").ok()?;
            let enc_destroy = *enc_lib.get::<EncDestroy>(b"tetra_enc_destroy").ok()?;
            let enc_encode = *enc_lib.get::<EncEncode>(b"tetra_enc_encode").ok()?;
            tracing::info!(dir = %dir.display(), "codec: ACELP libraries loaded");
            Some(CodecLib {
                _dec_lib: dec_lib,
                _enc_lib: enc_lib,
                dec_create,
                dec_destroy,
                dec_decode,
                enc_create,
                enc_destroy,
                enc_encode,
            })
        }
    }
}

/// Shared ETSI DSP/maths units used by both the decoder and encoder builds.
const SHARED_SRC: &[&str] = &[
    "sub_sc_d.c",
    "sub_dsp.c",
    "fbas_tet.c",
    "fexp_tet.c",
    "fmat_tet.c",
    "tetra_op.c",
];

/// Compile any missing codec library from the ETSI sources with clang, exactly
/// like the Python repo's `acelp.build_library` / `build_encoder_library`:
/// `clang -shared -O2 -I<etsi> -I<native> <sources> -o <lib>`. Does nothing if
/// the sources or clang are absent (the caller then falls back to no-voice).
fn ensure_built(dir: &Path, dec_path: &Path, enc_path: &Path) {
    let etsi = dir.join("etsi");
    if dec_path.exists() && enc_path.exists() {
        return;
    }
    if !etsi.join("source.h").exists() {
        tracing::info!(dir = %dir.display(), "codec: ETSI sources absent; cannot auto-build");
        return;
    }
    let clang = find_clang();
    let Some(clang) = clang else {
        tracing::warn!("codec: clang not found on PATH; cannot auto-build the ACELP libraries");
        return;
    };
    if !dec_path.exists() {
        let mut srcs: Vec<PathBuf> =
            std::iter::once("sdec_tet.c").chain(SHARED_SRC.iter().copied()).map(|s| etsi.join(s)).collect();
        srcs.push(dir.join("acelp_decode.c"));
        build_lib(&clang, &etsi, dir, &srcs, dec_path, "decoder");
    }
    if !enc_path.exists() {
        let mut srcs: Vec<PathBuf> =
            std::iter::once("scod_tet.c").chain(SHARED_SRC.iter().copied()).map(|s| etsi.join(s)).collect();
        srcs.push(dir.join("acelp_encode.c"));
        build_lib(&clang, &etsi, dir, &srcs, enc_path, "encoder");
    }
}

fn find_clang() -> Option<String> {
    for name in ["clang", "clang.exe"] {
        if std::process::Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(name.to_string());
        }
    }
    None
}

fn build_lib(clang: &str, etsi: &Path, native: &Path, srcs: &[PathBuf], out: &Path, kind: &str) {
    let mut cmd = std::process::Command::new(clang);
    cmd.arg("-shared").arg("-O2");
    #[cfg(not(windows))]
    cmd.arg("-fPIC");
    cmd.arg(format!("-I{}", etsi.display()));
    cmd.arg(format!("-I{}", native.display()));
    for s in srcs {
        cmd.arg(s);
    }
    cmd.arg("-o").arg(out);
    tracing::info!(kind, out = %out.display(), "codec: building ACELP library with clang");
    match cmd.output() {
        Ok(o) if o.status.success() && out.exists() => {
            tracing::info!(kind, "codec: build succeeded");
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            tracing::warn!(kind, status = ?o.status.code(), stderr = %err.trim(), "codec: build failed");
        }
        Err(e) => tracing::warn!(kind, error = %e, "codec: failed to run clang"),
    }
}

/// A per-call decoder context. Not Send: use it only on the thread that made it.
struct Decoder {
    codec: Arc<CodecLib>,
    ctx: *mut c_void,
}

impl Decoder {
    fn new(codec: Arc<CodecLib>) -> Option<Decoder> {
        let ctx = unsafe { (codec.dec_create)() };
        if ctx.is_null() {
            return None;
        }
        Some(Decoder { codec, ctx })
    }

    /// Decode a 274-bit speech frame into 480 PCM samples (concealing on `bad`).
    fn decode(&self, bits: &[u8], bad: bool) -> Option<[i16; FRAME_SAMPLES]> {
        if bits.len() != FRAME_BITS {
            return None;
        }
        let mut pcm = [0i16; FRAME_SAMPLES];
        let bfi = if bad { 1 } else { 0 };
        for sf in 0..2 {
            let inp = &bits[sf * SUBFRAME_BITS..(sf + 1) * SUBFRAME_BITS];
            let out = &mut pcm[sf * SUBFRAME_SAMPLES..(sf + 1) * SUBFRAME_SAMPLES];
            let rc = unsafe { (self.codec.dec_decode)(self.ctx, inp.as_ptr(), bfi, out.as_mut_ptr()) };
            if rc != 0 {
                return None;
            }
        }
        Some(pcm)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { (self.codec.dec_destroy)(self.ctx) };
    }
}

/// A per-call encoder context. Not Send: use it only on the encoder thread.
struct Encoder {
    codec: Arc<CodecLib>,
    ctx: *mut c_void,
}

impl Encoder {
    fn new(codec: Arc<CodecLib>) -> Option<Encoder> {
        let ctx = unsafe { (codec.enc_create)() };
        if ctx.is_null() {
            return None;
        }
        Some(Encoder { codec, ctx })
    }

    /// Encode 480 PCM samples into 274 codec bits (2 sub-frames).
    fn encode(&self, pcm: &[i16; FRAME_SAMPLES]) -> Option<Vec<u8>> {
        let mut bits = vec![0u8; FRAME_BITS];
        for sf in 0..2 {
            let inp = &pcm[sf * SUBFRAME_SAMPLES..(sf + 1) * SUBFRAME_SAMPLES];
            let out = &mut bits[sf * SUBFRAME_BITS..(sf + 1) * SUBFRAME_BITS];
            let rc = unsafe { (self.codec.enc_encode)(self.ctx, inp.as_ptr(), out.as_mut_ptr()) };
            if rc != 0 {
                return None;
            }
        }
        Some(bits)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { (self.codec.enc_destroy)(self.ctx) };
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
    decoder: Option<Decoder>,
    playback: Arc<Mutex<VecDeque<i16>>>, // output-rate mono
    out_rate: u32,
    uplink_active: Arc<AtomicBool>,
    uplink_cid: Arc<AtomicU32>,
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
        let dir = PathBuf::from(&cfg.codec_dir);
        let codec = Arc::new(CodecLib::load(&dir)?);

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

        let playback: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
        let uplink_active = Arc::new(AtomicBool::new(false));
        let uplink_cid = Arc::new(AtomicU32::new(0));

        let out_stream =
            build_output_stream(&out_dev, &out_cfg, out_channels, playback.clone())?;
        out_stream.play().ok()?;

        // Encoder thread: PCM frames -> ACELP bits -> MsUplinkSpeech via app loop.
        let (enc_tx, enc_rx) = crossbeam_channel::unbounded::<Vec<i16>>();
        {
            let codec = codec.clone();
            let cid = uplink_cid.clone();
            std::thread::Builder::new()
                .name("acelp-enc".into())
                .spawn(move || {
                    let Some(encoder) = Encoder::new(codec) else {
                        tracing::warn!("audio: encoder context init failed");
                        return;
                    };
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
        )?;
        in_stream.play().ok()?;

        let decoder = Decoder::new(codec.clone());
        if decoder.is_none() {
            tracing::warn!("audio: decoder context init failed; downlink muted");
        }

        Some(AudioEngine {
            decoder,
            playback,
            out_rate,
            uplink_active,
            uplink_cid,
            _out_stream: out_stream,
            _in_stream: in_stream,
        })
    }

    /// Decode a downlink speech frame and queue it for playback.
    pub fn play_downlink(&self, bits: &[u8], bad: bool) {
        let Some(dec) = &self.decoder else { return };
        let Some(pcm8k) = dec.decode(bits, bad) else { return };
        // Resample 8 kHz -> output rate with a fresh linear pass per frame.
        let mut rs = Resampler::new(CODEC_RATE, self.out_rate);
        let input: Vec<f32> = pcm8k.iter().map(|&s| s as f32).collect();
        let mut out = Vec::with_capacity((FRAME_SAMPLES * self.out_rate as usize) / 8000 + 4);
        rs.process(&input, &mut out);
        let mut q = self.playback.lock().unwrap();
        // Cap the buffer so a stalled reader can't grow it without bound (~1 s).
        let cap = self.out_rate as usize;
        for v in out {
            q.push_back(v.clamp(-32768.0, 32767.0) as i16);
        }
        while q.len() > cap {
            q.pop_front();
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
) -> Option<cpal::Stream> {
    let sample_format = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.clone().into();
    let err = |e| tracing::warn!(error = %e, "audio: output stream error");
    macro_rules! out {
        ($t:ty, $conv:expr) => {{
            dev.build_output_stream(
                &config,
                move |data: &mut [$t], _| {
                    let mut q = playback.lock().unwrap();
                    for frame in data.chunks_mut(channels) {
                        let s = q.pop_front().unwrap_or(0);
                        let v = $conv(s);
                        for slot in frame.iter_mut() {
                            *slot = v;
                        }
                    }
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
            let _ = enc_tx.send(frame);
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
