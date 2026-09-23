//! Generate a long "live recording" for trying the app: synthetic tracks separated by short
//! silent gaps, encoded as VBR MP3 (plus a short WAV).
//!
//!     cargo run -p splitter-audio --release --example make_test_recording -- testdata 60

use mp3lame_encoder::{Builder, FlushNoGap, InterleavedPcm, Quality, VbrMode};
use std::f32::consts::TAU;
use std::path::PathBuf;

const RATE: u32 = 44100;

/// A simple tune: a chord that changes every bar, a kick on each beat, some noise hats.
fn track(seconds: f32, seed: u32, out: &mut Vec<i16>) {
    let roots = [110.0, 130.8, 146.8, 164.8, 98.0, 123.5];
    let bpm = 90.0 + (seed % 5) as f32 * 12.0;
    let beat = 60.0 / bpm;
    let n = (seconds * RATE as f32) as usize;
    let mut rng = seed.wrapping_mul(747796405).wrapping_add(1);
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let bar = (t / (beat * 4.0)) as usize;
        let root = roots[(bar + seed as usize) % roots.len()];
        let chord = [1.0, 1.26, 1.5].iter().map(|m| (TAU * root * m * t).sin()).sum::<f32>() / 3.0;
        let bt = t % beat;
        let kick = (TAU * (60.0 + 90.0 * (-bt * 30.0).exp()) * bt).sin() * (-bt * 9.0).exp();
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = ((rng >> 16) as f32 / 65535.0 - 0.5) * (-(t % (beat / 2.0)) * 40.0).exp();
        let fade = (t / 1.5).min(1.0).min((seconds - t) / 2.0).max(0.0);
        let l = (chord * 0.35 + kick * 0.5 + noise * 0.25) * fade;
        let r = (chord * 0.35 + kick * 0.5 - noise * 0.2) * fade;
        out.push((l * 26000.0) as i16);
        out.push((r * 26000.0) as i16);
    }
}

fn silence(seconds: f32, out: &mut Vec<i16>) {
    out.extend(std::iter::repeat_n(0i16, (seconds * RATE as f32) as usize * 2));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or_else(|| "testdata".into()));
    let minutes: f32 = args.next().and_then(|m| m.parse().ok()).unwrap_or(60.0);
    std::fs::create_dir_all(&dir).unwrap();

    let mut pcm = Vec::new();
    let mut seed = 1;
    while (pcm.len() / 2) as f32 / (RATE as f32) < minutes * 60.0 {
        track(150.0 + (seed * 37 % 120) as f32, seed, &mut pcm);
        silence(2.0, &mut pcm);
        seed += 1;
    }
    println!("{} tracks, {:.1} min", seed - 1, pcm.len() as f32 / 2.0 / RATE as f32 / 60.0);

    let mut b = Builder::new().unwrap();
    b.set_num_channels(2).unwrap();
    b.set_sample_rate(RATE).unwrap();
    b.set_vbr_mode(VbrMode::Mtrh).unwrap();
    b.set_vbr_quality(Quality::Good).unwrap();
    b.set_quality(Quality::Good).unwrap();
    b.set_to_write_vbr_tag(true).unwrap();
    let mut enc = b.build().unwrap();
    let mut mp3 = Vec::new();
    for chunk in pcm.chunks(8192 * 2) {
        mp3.reserve(mp3lame_encoder::max_required_buffer_size(chunk.len()));
        enc.encode_to_vec(InterleavedPcm(chunk), &mut mp3).unwrap();
    }
    mp3.reserve(7200);
    enc.flush_to_vec::<FlushNoGap>(&mut mp3).unwrap();
    let mut tag = Vec::with_capacity(enc.lame_tag_size());
    if enc.lame_tag_encode_to_vec(&mut tag).is_some() {
        mp3[..tag.len()].copy_from_slice(&tag);
    }
    let path = dir.join(format!("live-set-{}min.mp3", minutes as u32));
    std::fs::write(&path, &mp3).unwrap();
    println!("wrote {} ({:.1} MB)", path.display(), mp3.len() as f64 / 1e6);

    // A short WAV too, to exercise the non-MP3 path.
    let wav: Vec<i16> = pcm[..(RATE as usize * 2 * 200).min(pcm.len())].to_vec();
    let data_len = (wav.len() * 2) as u32;
    let mut f = Vec::with_capacity(44 + wav.len() * 2);
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data_len).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&RATE.to_le_bytes());
    f.extend_from_slice(&(RATE * 4).to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&data_len.to_le_bytes());
    for s in wav {
        f.extend_from_slice(&s.to_le_bytes());
    }
    let path = dir.join("rehearsal-excerpt.wav");
    std::fs::write(&path, f).unwrap();
    println!("wrote {}", path.display());
}
