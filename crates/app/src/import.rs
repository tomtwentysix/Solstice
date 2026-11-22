use solstice_engine::audio::write_wav;
use solstice_engine::resample::resample;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "oga", "m4a", "aif", "aiff", "caf"];

pub fn is_audio(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// Decodes any supported file, resamples to `sample_rate` and writes a float WAV named after
/// the source into `dir`. Returns the new file name and its length in frames.
pub fn import(src: &Path, dir: &Path, sample_rate: u32) -> Result<(String, u64), String> {
    let file = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = src.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("unsupported file: {e}"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track")?.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let src_rate = track.codec_params.sample_rate.unwrap_or(sample_rate);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let spec = *decoded.spec();
        let nch = spec.channels.count().clamp(1, 2);
        if channels.is_empty() {
            channels = vec![Vec::new(); nch];
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        let all = spec.channels.count();
        for frame in buf.samples().chunks(all) {
            for (c, ch) in channels.iter_mut().enumerate() {
                ch.push(frame[c.min(all - 1)]);
            }
        }
    }
    if channels.is_empty() || channels[0].is_empty() {
        return Err("file contains no audio".into());
    }
    if src_rate != sample_rate {
        channels = channels.iter().map(|c| resample(c, src_rate, sample_rate)).collect();
    }
    let stem = src.file_stem().map_or("Audio".into(), |s| s.to_string_lossy().into_owned());
    let mut name = format!("{stem}.wav");
    let mut n = 1;
    while dir.join(&name).exists() {
        n += 1;
        name = format!("{stem} {n}.wav");
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    write_wav(&dir.join(&name), sample_rate, &channels).map_err(|e| e.to_string())?;
    Ok((name, channels[0].len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-bit big-endian mono PCM at 44.1 kHz; AIFF's rate is an 80-bit extended float.
    fn aiff(path: &Path, samples: &[i16]) {
        let mut comm = 1i16.to_be_bytes().to_vec();
        comm.extend((samples.len() as u32).to_be_bytes());
        comm.extend(16i16.to_be_bytes());
        comm.extend([0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
        let mut ssnd = vec![0u8; 8];
        ssnd.extend(samples.iter().flat_map(|s| s.to_be_bytes()));
        let mut body = b"AIFF".to_vec();
        for (id, data) in [(b"COMM", comm), (b"SSND", ssnd)] {
            body.extend(id);
            body.extend((data.len() as u32).to_be_bytes());
            body.extend(data);
        }
        let mut file = b"FORM".to_vec();
        file.extend((body.len() as u32).to_be_bytes());
        file.extend(body);
        std::fs::write(path, file).unwrap();
    }

    /// 16-bit little-endian stereo linear PCM at 48 kHz.
    fn caf(path: &Path, frames: usize) {
        let mut file = b"caff".to_vec();
        file.extend([0, 1, 0, 0]);
        let mut desc = 48000f64.to_be_bytes().to_vec();
        desc.extend(b"lpcm");
        for v in [2u32, 4, 1, 2, 16] {
            desc.extend(v.to_be_bytes());
        }
        let mut data = vec![0u8; 4];
        data.extend((0..frames * 2).flat_map(|i| ((i % 3000) as i16 * 10).to_le_bytes()));
        for (id, chunk) in [(b"desc", desc), (b"data", data)] {
            file.extend(id);
            file.extend((chunk.len() as i64).to_be_bytes());
            file.extend(chunk);
        }
        std::fs::write(path, file).unwrap();
    }

    #[test]
    fn imports_aiff_and_caf() {
        let dir = std::env::temp_dir().join(format!("solstice-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        aiff(&dir.join("Gtr#01.aif"), &[0, 1000, -1000, 2000].repeat(1000));
        caf(&dir.join("Vox#01.caf"), 4800);
        let out = dir.join("out");
        assert_eq!(import(&dir.join("Vox#01.caf"), &out, 48000), Ok(("Vox#01.wav".into(), 4800)));
        let (name, frames) = import(&dir.join("Gtr#01.aif"), &out, 48000).unwrap();
        assert_eq!(name, "Gtr#01.wav");
        // 4000 frames at 44.1 kHz resampled to 48 kHz.
        assert!(frames.abs_diff(4354) <= 2, "{frames}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
