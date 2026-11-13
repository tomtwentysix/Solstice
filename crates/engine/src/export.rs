//! Stereo audio files for export: WAV at 16 or 24 bits or 32-bit float, or FLAC at 16 or 24
//! bits. Integer formats go through the dither stage, the only place a mix is quantised.

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context, Fill, FrameBuf};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Wav16,
    #[default]
    Wav24,
    WavFloat,
    Flac16,
    Flac24,
}

impl Format {
    pub const ALL: [Format; 5] = [Format::Wav16, Format::Wav24, Format::WavFloat, Format::Flac16, Format::Flac24];

    pub fn name(self) -> &'static str {
        match self {
            Format::Wav16 => "WAV 16-bit",
            Format::Wav24 => "WAV 24-bit",
            Format::WavFloat => "WAV 32-bit Float",
            Format::Flac16 => "FLAC 16-bit",
            Format::Flac24 => "FLAC 24-bit",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Wav16 | Format::Wav24 | Format::WavFloat => "wav",
            Format::Flac16 | Format::Flac24 => "flac",
        }
    }

    /// Bits per sample of an integer format, `None` for float.
    pub fn bits(self) -> Option<u32> {
        match self {
            Format::Wav16 | Format::Flac16 => Some(16),
            Format::Wav24 | Format::Flac24 => Some(24),
            Format::WavFloat => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dither {
    None,
    #[default]
    Tpdf,
    /// TPDF with second-order error feedback, moving the noise up towards Nyquist.
    Shaped,
}

impl Dither {
    pub const ALL: [Dither; 3] = [Dither::None, Dither::Tpdf, Dither::Shaped];

    pub fn name(self) -> &'static str {
        match self {
            Dither::None => "None",
            Dither::Tpdf => "TPDF",
            Dither::Shaped => "TPDF with Noise Shaping",
        }
    }
}

/// Turns float samples into integers of `bits`, in LSBs of that size.
pub struct Quantiser {
    scale: f32,
    dither: Dither,
    rng: u32,
    /// The last two quantisation errors per channel, for noise shaping.
    err: [[f32; 2]; 2],
}

impl Quantiser {
    pub fn new(bits: u32, dither: Dither) -> Quantiser {
        Quantiser { scale: (1u32 << (bits - 1)) as f32, dither, rng: 0x9e37_79b9, err: [[0.0; 2]; 2] }
    }

    fn uniform(&mut self) -> f32 {
        (solstice_model::xorshift(&mut self.rng) >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Triangular noise between −1 and +1 LSB.
    pub fn tpdf(&mut self) -> f32 {
        self.uniform() - self.uniform()
    }

    pub fn run(&mut self, ch: usize, x: f32) -> i32 {
        let v = x * self.scale;
        let [e1, e2] = self.err[ch];
        let w = if self.dither == Dither::Shaped { v - (2.0 * e1 - e2) } else { v };
        let d = if self.dither == Dither::None { 0.0 } else { self.tpdf() };
        let q = (w + d).round().clamp(-self.scale, self.scale - 1.0);
        if self.dither == Dither::Shaped {
            // Held to what dither and rounding can give, so clipping can't run the feedback away.
            self.err[ch] = [(q - w).clamp(-1.5, 1.5), e1];
        }
        q as i32
    }
}

type Wav = hound::WavWriter<BufWriter<File>>;

/// FLAC frames are written as they fill; the STREAMINFO block at the start, which needs the
/// totals and MD5, is written again over its placeholder at the end.
struct Flac {
    file: BufWriter<File>,
    config: Verified<flacenc::config::Encoder>,
    info: StreamInfo,
    context: Context,
    frame: FrameBuf,
    pending: Vec<i32>,
    sink: ByteSink,
}

impl Flac {
    fn create(path: &Path, sr: u32, bits: u32) -> io::Result<Flac> {
        let config = flacenc::config::Encoder::default().into_verified().map_err(|(_, e)| io::Error::other(format!("{e:?}")))?;
        let block = config.block_size;
        let flac = |e: flacenc::error::VerifyError| io::Error::other(format!("{e:?}"));
        let mut info = StreamInfo::new(sr as usize, 2, bits as usize).map_err(flac)?;
        info.set_block_sizes(block, block).map_err(flac)?;
        let mut f = Flac {
            file: BufWriter::new(File::create(path)?),
            config,
            info,
            context: Context::new(bits as usize, 2),
            frame: FrameBuf::with_size(2, block).map_err(flac)?,
            pending: Vec::with_capacity(block * 2),
            sink: ByteSink::new(),
        };
        f.write_header()?;
        Ok(f)
    }

    fn write_header(&mut self) -> io::Result<()> {
        self.sink.clear();
        self.info.write(&mut self.sink).map_err(|e| io::Error::other(format!("{e:?}")))?;
        let len = self.sink.as_slice().len() as u32;
        self.file.write_all(b"fLaC")?;
        // The last (and only) metadata block, of type STREAMINFO.
        self.file.write_all(&[0x80, (len >> 16) as u8, (len >> 8) as u8, len as u8])?;
        self.file.write_all(self.sink.as_slice())
    }

    fn push(&mut self, l: i32, r: i32) -> io::Result<()> {
        self.pending.extend_from_slice(&[l, r]);
        if self.pending.len() == self.pending.capacity() { self.flush() } else { Ok(()) }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let err = |e: flacenc::error::SourceError| io::Error::other(format!("{e:?}"));
        self.frame.fill_interleaved(&self.pending).map_err(err)?;
        self.context.fill_interleaved(&self.pending).map_err(err)?;
        self.pending.clear();
        let n = self.context.current_frame_number().unwrap_or(0);
        let frame = flacenc::encode_fixed_size_frame(&self.config, &self.frame, n, &self.info)
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
        self.info.update_frame_info(&frame);
        self.sink.clear();
        frame.write(&mut self.sink).map_err(|e| io::Error::other(format!("{e:?}")))?;
        self.file.write_all(self.sink.as_slice())
    }

    fn finish(mut self) -> io::Result<()> {
        self.flush()?;
        let block = self.config.block_size;
        // As the reference encoder: the last, shorter block doesn't count towards the minimum.
        self.info.set_block_sizes(block, block).map_err(|e| io::Error::other(format!("{e:?}")))?;
        self.info.set_md5_digest(&self.context.md5_digest());
        self.file.rewind()?;
        self.write_header()?;
        self.file.flush()
    }
}

enum Sink {
    Wav(Wav),
    Flac(Flac),
}

/// Writes a stereo file beside `path` and renames it into place on `finish`, so a cancelled or
/// failed export leaves nothing behind.
pub struct Writer {
    sink: Option<Sink>,
    quantiser: Option<Quantiser>,
    part: PathBuf,
    path: PathBuf,
}

impl Writer {
    pub fn create(path: &Path, sr: u32, format: Format, dither: Dither) -> io::Result<Writer> {
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let wav = |bits: u16, sample_format| hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: bits, sample_format };
        let sink = match format {
            Format::WavFloat => Sink::Wav(Wav::create(&part, crate::audio::wav_spec(2, sr)).map_err(io::Error::other)?),
            Format::Wav16 | Format::Wav24 => {
                let bits = format.bits().unwrap_or(24) as u16;
                Sink::Wav(Wav::create(&part, wav(bits, hound::SampleFormat::Int)).map_err(io::Error::other)?)
            }
            Format::Flac16 | Format::Flac24 => Sink::Flac(Flac::create(&part, sr, format.bits().unwrap_or(24))?),
        };
        Ok(Writer { sink: Some(sink), quantiser: format.bits().map(|b| Quantiser::new(b, dither)), part, path: path.to_owned() })
    }

    pub fn write(&mut self, l: &[f32], r: &[f32]) -> io::Result<()> {
        let frames = l.iter().zip(r);
        match (self.sink.as_mut(), self.quantiser.as_mut()) {
            (Some(Sink::Wav(w)), None) => {
                for (&a, &b) in frames {
                    w.write_sample(a).map_err(io::Error::other)?;
                    w.write_sample(b).map_err(io::Error::other)?;
                }
            }
            (Some(Sink::Wav(w)), Some(q)) => {
                for (&a, &b) in frames {
                    w.write_sample(q.run(0, a)).map_err(io::Error::other)?;
                    w.write_sample(q.run(1, b)).map_err(io::Error::other)?;
                }
            }
            (Some(Sink::Flac(f)), Some(q)) => {
                for (&a, &b) in frames {
                    f.push(q.run(0, a), q.run(1, b))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let done = match self.sink.take() {
            Some(Sink::Wav(w)) => w.finalize().map_err(io::Error::other),
            Some(Sink::Flac(f)) => f.finish(),
            None => Ok(()),
        }
        .and_then(|()| std::fs::rename(&self.part, &self.path));
        if done.is_err() {
            let _ = std::fs::remove_file(&self.part);
        }
        done
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if self.sink.take().is_some() {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

/// Copies a 32-bit float stereo WAV into `format` at `gain`. Returns false if cancelled.
pub fn convert(src: &Path, dst: &Path, format: Format, dither: Dither, gain: f32, cancel: &AtomicBool) -> io::Result<bool> {
    let mut reader = hound::WavReader::open(src).map_err(io::Error::other)?;
    let mut out = Writer::create(dst, reader.spec().sample_rate, format, dither)?;
    const CHUNK: usize = 8192;
    let (mut l, mut r) = (Vec::with_capacity(CHUNK), Vec::with_capacity(CHUNK));
    let mut samples = reader.samples::<f32>();
    loop {
        if cancel.load(Relaxed) {
            return Ok(false);
        }
        l.clear();
        r.clear();
        while l.len() < CHUNK {
            let (Some(a), Some(b)) = (samples.next(), samples.next()) else { break };
            l.push(a.map_err(io::Error::other)? * gain);
            r.push(b.map_err(io::Error::other)? * gain);
        }
        if l.is_empty() {
            break;
        }
        out.write(&l, &r)?;
    }
    out.finish()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("solstice-export-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn signal(n: usize) -> (Vec<f32>, Vec<f32>) {
        let l = (0..n).map(|i| 0.5 * (i as f32 * 0.01).sin()).collect();
        let r = (0..n).map(|i| -0.25 * (i as f32 * 0.003).cos()).collect();
        (l, r)
    }

    #[test]
    fn tpdf_dither_is_bounded_and_unbiased() {
        let mut q = Quantiser::new(16, Dither::Tpdf);
        let n = 200_000;
        let (mut sum, mut sq) = (0.0f64, 0.0f64);
        for i in 0..n {
            let d = q.tpdf();
            assert!(d.abs() < 1.0, "TPDF noise stays within ±1 LSB");
            let x = 0.3 * (i as f32 * 0.001).sin();
            let err = q.run(0, x) as f64 - x as f64 * 32768.0;
            assert!(err.abs() <= 1.5 + 1e-3, "{err}");
            sum += err;
            sq += err * err;
        }
        let (mean, var) = (sum / n as f64, sq / n as f64);
        assert!(mean.abs() < 0.01, "{mean}");
        // Dither (1/6 LSB²) plus rounding (1/12 LSB²).
        assert!((var - 0.25).abs() < 0.02, "{var}");
    }

    #[test]
    fn noise_shaping_moves_the_error_up() {
        let mut q = Quantiser::new(16, Dither::Shaped);
        let x = 0.1f32;
        let errs: Vec<f64> = (0..50_000).map(|_| q.run(0, x) as f64 - x as f64 * 32768.0).collect();
        let mean = errs.iter().sum::<f64>() / errs.len() as f64;
        assert!(mean.abs() < 0.01, "{mean}");
        // Shaped by (1 − z⁻¹)², neighbouring errors are strongly anti-correlated.
        let lag: f64 = errs.windows(2).map(|w| w[0] * w[1]).sum::<f64>() / errs.iter().map(|e| e * e).sum::<f64>();
        assert!(lag < -0.5, "{lag}");
        let mut clip = Quantiser::new(16, Dither::Shaped);
        assert!((0..1000).all(|_| clip.run(0, 2.0) == 32767), "clipping stays stable");
        assert!(clip.run(0, 0.0).abs() < 8);
    }

    #[test]
    fn wav_integer_round_trip() {
        let d = dir("wav");
        let (l, r) = signal(5000);
        for (format, bits) in [(Format::Wav16, 16), (Format::Wav24, 24)] {
            let path = d.join(format!("{bits}.wav"));
            let mut w = Writer::create(&path, 44100, format, Dither::None).unwrap();
            w.write(&l[..3000], &r[..3000]).unwrap();
            w.write(&l[3000..], &r[3000..]).unwrap();
            assert!(!path.exists(), "written beside the file until finished");
            w.finish().unwrap();
            let mut reader = hound::WavReader::open(&path).unwrap();
            let spec = reader.spec();
            assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (2, 44100, bits as u16));
            assert_eq!(spec.sample_format, hound::SampleFormat::Int);
            let got: Vec<i32> = reader.samples::<i32>().map(|s| s.unwrap()).collect();
            assert_eq!(got.len(), 10000);
            let scale = (1 << (bits - 1)) as f32;
            for i in 0..5000 {
                assert_eq!(got[2 * i], (l[i] * scale).round() as i32);
                assert_eq!(got[2 * i + 1], (r[i] * scale).round() as i32);
            }
        }
    }

    #[test]
    fn flac_round_trip() {
        let d = dir("flac");
        // Longer than one FLAC block, ending part-way through another.
        let (l, r) = signal(10_000);
        for (format, bits) in [(Format::Flac16, 16u32), (Format::Flac24, 24)] {
            let path = d.join(format!("{bits}.flac"));
            let mut w = Writer::create(&path, 48000, format, Dither::None).unwrap();
            w.write(&l, &r).unwrap();
            w.finish().unwrap();
            let mut reader = claxon::FlacReader::open(&path).unwrap();
            let info = reader.streaminfo();
            assert_eq!((info.channels, info.sample_rate, info.bits_per_sample), (2, 48000, bits));
            assert_eq!(info.samples, Some(10_000));
            let got: Vec<i32> = reader.samples().map(|s| s.unwrap()).collect();
            assert_eq!(got.len(), 20_000);
            let scale = (1 << (bits - 1)) as f32;
            for i in 0..10_000 {
                assert_eq!(got[2 * i], (l[i] * scale).round() as i32);
                assert_eq!(got[2 * i + 1], (r[i] * scale).round() as i32);
            }
        }
    }

    #[test]
    fn unfinished_files_are_removed() {
        let d = dir("drop");
        let path = d.join("a.flac");
        let mut w = Writer::create(&path, 48000, Format::Flac16, Dither::Tpdf).unwrap();
        w.write(&[0.1; 100], &[0.1; 100]).unwrap();
        drop(w);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    }

    #[test]
    fn a_failed_finish_removes_the_part_file() {
        let d = dir("finish");
        let path = d.join("a.wav");
        let mut w = Writer::create(&path, 48000, Format::Wav24, Dither::Tpdf).unwrap();
        w.write(&[0.1; 100], &[0.1; 100]).unwrap();
        // A folder in the way makes the rename fail.
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("x"), b"x").unwrap();
        assert!(w.finish().is_err());
        assert!(!d.join("a.wav.part").exists());
    }

    #[test]
    fn convert_applies_gain() {
        let d = dir("convert");
        let src = d.join("float.wav");
        let (l, r) = signal(20_000);
        let mut w = Writer::create(&src, 48000, Format::WavFloat, Dither::Tpdf).unwrap();
        w.write(&l, &r).unwrap();
        w.finish().unwrap();
        let dst = d.join("out.wav");
        assert!(convert(&src, &dst, Format::Wav24, Dither::None, 0.5, &AtomicBool::new(false)).unwrap());
        let got: Vec<i32> = hound::WavReader::open(&dst).unwrap().samples::<i32>().map(|s| s.unwrap()).collect();
        assert_eq!(got.len(), 40_000);
        assert_eq!(got[2 * 1234], (l[1234] * 0.5 * 8_388_608.0).round() as i32);
        assert!(!convert(&src, &d.join("no.wav"), Format::Wav16, Dither::None, 1.0, &AtomicBool::new(true)).unwrap());
        assert!(!d.join("no.wav").exists() && !d.join("no.wav.part").exists());
    }
}
