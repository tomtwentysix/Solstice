use solstice_engine::audio::write_wav;
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

/// Linear interpolation. Adequate for previews and mismatched imports; not mastering grade.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    let ratio = from as f64 / to as f64;
    let len = (input.len() as f64 / ratio) as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let f = (pos - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * f
        })
        .collect()
}
