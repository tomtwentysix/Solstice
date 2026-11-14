use memmap2::Mmap;
use std::fs::File;
use std::io;
use std::path::Path;

/// A 32-bit float WAV mapped into memory. All project audio is stored in this format so
/// playback reads samples straight out of the page cache with no decoding.
pub struct AudioData {
    map: Mmap,
    data_offset: usize,
    pub channels: usize,
    pub frames: u64,
    pub sample_rate: u32,
}

impl AudioData {
    pub fn open(path: &Path) -> io::Result<AudioData> {
        let file = File::open(path)?;
        let map = unsafe { Mmap::map(&file)? };
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {m}", path.display()));
        if map.len() < 12 || &map[0..4] != b"RIFF" || &map[8..12] != b"WAVE" {
            return Err(bad("not a WAV file"));
        }
        let mut pos = 12;
        let mut fmt = None;
        while pos + 8 <= map.len() {
            let id = &map[pos..pos + 4];
            let size = u32::from_le_bytes(map[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let body = pos + 8;
            if id == b"fmt " && size >= 16 {
                if body + 16 > map.len() {
                    return Err(bad("the file is cut short"));
                }
                let tag = u16::from_le_bytes([map[body], map[body + 1]]);
                let channels = u16::from_le_bytes([map[body + 2], map[body + 3]]) as usize;
                let rate = u32::from_le_bytes(map[body + 4..body + 8].try_into().unwrap());
                let bits = u16::from_le_bytes([map[body + 14], map[body + 15]]);
                // 3 = IEEE float, 0xFFFE = WAVE_FORMAT_EXTENSIBLE (hound writes this for float).
                if !(tag == 3 || tag == 0xFFFE) || bits != 32 {
                    return Err(bad("audio files must be 32-bit float"));
                }
                if channels == 0 || rate == 0 {
                    return Err(bad("the file's header is damaged"));
                }
                fmt = Some((channels, rate));
            } else if id == b"data" {
                let (channels, sample_rate) = fmt.ok_or_else(|| bad("data before fmt"))?;
                // A file still being recorded has a placeholder size; trust the file length.
                let avail = map.len() - body;
                let len = if size == 0 || size > avail { avail } else { size };
                let frames = (len / 4 / channels.max(1)) as u64;
                let _ = map.advise(memmap2::Advice::Sequential);
                return Ok(AudioData { map, data_offset: body, channels, frames, sample_rate });
            }
            pos = body.saturating_add(size).saturating_add(size & 1);
        }
        Err(bad("no data chunk"))
    }

    #[inline]
    pub fn sample(&self, channel: usize, frame: u64) -> f32 {
        let i = self.data_offset + ((frame as usize * self.channels + channel) * 4);
        f32::from_le_bytes(self.map[i..i + 4].try_into().unwrap())
    }

    /// Asks the kernel to read a span ahead of the playhead so the RT thread doesn't fault.
    pub fn prefetch(&self, frame: u64, frames: u64) {
        let start = self.data_offset + frame.min(self.frames) as usize * self.channels * 4;
        let len = (frames.min(self.frames.saturating_sub(frame)) as usize) * self.channels * 4;
        let page = 4096;
        let aligned = start & !(page - 1);
        if len > 0 {
            let _ = self.map.advise_range(memmap2::Advice::WillNeed, aligned, len + (start - aligned));
        }
    }

    /// Reads one byte of every page in a span, so it is in memory before the audio thread
    /// needs it: the read waits for the disk here instead. Returns the pages touched.
    pub fn touch(&self, frame: u64, frames: u64) -> usize {
        let frame = frame.min(self.frames);
        let frames = frames.min(self.frames - frame);
        if frames == 0 {
            return 0;
        }
        let start = self.data_offset + frame as usize * self.channels * 4;
        let end = start + frames as usize * self.channels * 4;
        let page = 4096;
        let mut n = 0;
        let mut at = start;
        while at < end {
            unsafe { std::ptr::read_volatile(self.map.as_ptr().add(at)) };
            n += 1;
            at = (at / page + 1) * page;
        }
        n
    }

    /// Min/max pairs per `block` frames for the given channel, used for waveform drawing.
    pub fn peaks(&self, channel: usize, block: usize) -> Vec<(f32, f32)> {
        let ch = channel.min(self.channels - 1);
        let mut out = Vec::with_capacity(self.frames as usize / block + 1);
        let mut f = 0u64;
        while f < self.frames {
            let end = (f + block as u64).min(self.frames);
            let (mut lo, mut hi) = (0f32, 0f32);
            for i in f..end {
                let s = self.sample(ch, i);
                lo = lo.min(s);
                hi = hi.max(s);
            }
            out.push((lo, hi));
            f = end;
        }
        out
    }
}

pub fn wav_spec(channels: u16, sample_rate: u32) -> hound::WavSpec {
    hound::WavSpec { channels, sample_rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float }
}

/// Writes planar float data as a 32-bit float WAV.
pub fn write_wav(path: &Path, sample_rate: u32, channels: &[Vec<f32>]) -> io::Result<()> {
    let mut w = hound::WavWriter::create(path, wav_spec(channels.len() as u16, sample_rate))
        .map_err(io::Error::other)?;
    let frames = channels.first().map_or(0, |c| c.len());
    for f in 0..frames {
        for c in channels {
            w.write_sample(c[f]).map_err(io::Error::other)?;
        }
    }
    w.finalize().map_err(io::Error::other)
}
