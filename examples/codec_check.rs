// Diagnostic: load the ACELP libraries from ./native and round-trip a tone
// through encode -> decode, printing input/output RMS to confirm the codec
// works over FFI. Run: cargo run --example codec_check
use libloading::{Library, Symbol};
use std::ffi::c_void;

type EncCreate = unsafe extern "C" fn() -> *mut c_void;
type EncEncode = unsafe extern "C" fn(*mut c_void, *const i16, *mut u8) -> i32;
type DecCreate = unsafe extern "C" fn() -> *mut c_void;
type DecDecode = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut i16) -> i32;

fn rms(s: &[i16]) -> f64 {
    let sum: f64 = s.iter().map(|&v| (v as f64) * (v as f64)).sum();
    (sum / s.len() as f64).sqrt()
}

fn main() {
    let dir = std::path::Path::new("native");
    let enc_name = if cfg!(windows) { "tetra_acelp_enc.dll" } else { "libtetra_acelp_enc.so" };
    let dec_name = if cfg!(windows) { "tetra_acelp.dll" } else { "libtetra_acelp.so" };
    unsafe {
        let enc_lib = Library::new(dir.join(enc_name)).expect("enc lib");
        let dec_lib = Library::new(dir.join(dec_name)).expect("dec lib");
        let enc_create: Symbol<EncCreate> = enc_lib.get(b"tetra_enc_create").unwrap();
        let enc_encode: Symbol<EncEncode> = enc_lib.get(b"tetra_enc_encode").unwrap();
        let dec_create: Symbol<DecCreate> = dec_lib.get(b"tetra_dec_create").unwrap();
        let dec_decode: Symbol<DecDecode> = dec_lib.get(b"tetra_dec_decode").unwrap();

        let enc = enc_create();
        let dec = dec_create();

        let mut phase = 0.0f64;
        let step = 2.0 * std::f64::consts::PI * 440.0 / 8000.0;
        let mut in_rms = 0.0;
        let mut out_rms = 0.0;
        let mut frames = 0;
        for _ in 0..33 {
            let mut pcm = [0i16; 240];
            for s in pcm.iter_mut() {
                *s = (phase.sin() * 8000.0) as i16;
                phase += step;
            }
            let mut bits = [0u8; 137];
            let rc_e = enc_encode(enc, pcm.as_ptr(), bits.as_mut_ptr());
            let ones: i32 = bits.iter().map(|&b| b as i32).sum();
            let mut out = [0i16; 240];
            let rc_d = dec_decode(dec, bits.as_ptr(), 0, out.as_mut_ptr());
            if rc_e != 0 || rc_d != 0 {
                println!("rc_e={rc_e} rc_d={rc_d}");
            }
            in_rms += rms(&pcm);
            out_rms += rms(&out);
            frames += 1;
            if frames <= 3 {
                println!(
                    "frame {frames}: bits_set={ones}/137 in_rms={:.0} out_rms={:.0}",
                    rms(&pcm),
                    rms(&out)
                );
            }
        }
        println!(
            "avg in_rms={:.0} avg out_rms={:.0} over {frames} frames",
            in_rms / frames as f64,
            out_rms / frames as f64
        );
        println!(
            "{}",
            if out_rms > 100.0 {
                "CODEC OK: non-silent output"
            } else {
                "CODEC PROBLEM: output is ~silent"
            }
        );
    }
}
