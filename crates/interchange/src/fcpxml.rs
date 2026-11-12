//! Final Cut Pro XML, as exported by audio and video editors.
//! Times are rational seconds ("1001/30000s", "5s"); a clip's `offset` is in its parent's
//! timeline and its `start` is the in-point in its source.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub name: String,
    /// Media location as written, usually a `file://` URL from the Mac it was exported on.
    pub src: String,
    /// Position on the project timeline, in seconds.
    pub start: f64,
    /// In-point into the source file, in seconds.
    pub offset: f64,
    pub duration: f64,
    pub gain_db: f32,
    pub fade_in: f64,
    pub fade_out: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lane {
    pub name: String,
    pub clips: Vec<Clip>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arrangement {
    pub name: String,
    pub lanes: Vec<Lane>,
}

/// Parses "a/bs", "ns" or "n" into seconds.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('s');
    match s.split_once('/') {
        Some((a, b)) => {
            let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
            (b != 0.0).then(|| a / b)
        }
        None => s.parse().ok(),
    }
}

fn time_attr(n: roxmltree::Node, name: &str) -> f64 {
    n.attribute(name).and_then(parse_time).unwrap_or(0.0)
}

fn parse_db(s: &str) -> Option<f32> {
    s.trim().trim_end_matches("dB").trim().parse().ok()
}

struct Asset {
    src: String,
    start: f64,
    name: String,
}

const CONTAINERS: [&str; 6] = ["gap", "clip", "ref-clip", "sync-clip", "mc-clip", "spine"];
const MEDIA: [&str; 3] = ["asset-clip", "audio", "video"];

struct Walker<'a> {
    assets: &'a HashMap<String, Asset>,
    lanes: Vec<(String, Lane)>,
}

impl Walker<'_> {
    fn lane(&mut self, key: String) -> &mut Lane {
        if let Some(i) = self.lanes.iter().position(|(k, _)| *k == key) {
            return &mut self.lanes[i].1;
        }
        let name = key.rsplit('.').next().unwrap_or(&key).to_string();
        self.lanes.push((key, Lane { name, clips: Vec::new() }));
        &mut self.lanes.last_mut().unwrap().1
    }

    /// `base` is where the parent's timeline starts on the project timeline, and
    /// `parent_start` the parent's own start time, which children's offsets are measured in.
    fn walk(&mut self, node: roxmltree::Node, base: f64, parent_start: f64, lane: &str, role: &str) {
        for child in node.children().filter(|c| c.is_element()) {
            let tag = child.tag_name().name();
            let is_media = MEDIA.contains(&tag);
            if !is_media && !CONTAINERS.contains(&tag) {
                continue;
            }
            let at = base + (time_attr(child, "offset") - parent_start);
            let start = time_attr(child, "start");
            let lane = child.attribute("lane").unwrap_or(lane).to_string();
            let role = child.attribute("audioRole").or(child.attribute("role")).unwrap_or(role).to_string();
            if is_media
                && let Some(asset) = child.attribute("ref").and_then(|r| self.assets.get(r))
            {
                let (gain_db, fade_in, fade_out) = volume(child);
                let name = child.attribute("name").map(str::to_string).unwrap_or_else(|| asset.name.clone());
                let clip = Clip {
                    name,
                    src: asset.src.clone(),
                    start: at,
                    offset: (start - asset.start).max(0.0),
                    duration: time_attr(child, "duration"),
                    gain_db,
                    fade_in,
                    fade_out,
                };
                let key = if role.is_empty() { format!("Lane {lane}") } else { role.clone() };
                self.lane(key).clips.push(clip);
            }
            // Containers and media both carry connected clips, timed against their own start.
            self.walk(child, at, start, &lane, &role);
        }
    }
}

/// Gain and fade lengths from an `adjust-volume` child.
fn volume(clip: roxmltree::Node) -> (f32, f64, f64) {
    let Some(adj) = clip.children().find(|c| c.has_tag_name("adjust-volume")) else { return (0.0, 0.0, 0.0) };
    let gain = adj.attribute("amount").and_then(parse_db).unwrap_or(0.0);
    let mut fades = (0.0, 0.0);
    for f in adj.descendants() {
        match f.tag_name().name() {
            "fadeIn" => fades.0 = time_attr(f, "duration"),
            "fadeOut" => fades.1 = time_attr(f, "duration"),
            _ => {}
        }
    }
    (gain, fades.0, fades.1)
}

pub fn parse(xml: &str) -> Result<Arrangement, String> {
    // Real exports start with <!DOCTYPE fcpxml>, which roxmltree refuses unless allowed.
    let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(xml, opts).map_err(|e| format!("not valid XML: {e}"))?;
    let root = doc.root_element();
    if !root.has_tag_name("fcpxml") {
        return Err("not a Final Cut Pro XML file".into());
    }
    let mut assets = HashMap::new();
    for a in root.descendants().filter(|n| n.has_tag_name("asset")) {
        let Some(id) = a.attribute("id") else { continue };
        // Older files put the location on the asset; newer ones in a media-rep child.
        let src = a
            .attribute("src")
            .or_else(|| a.children().find(|c| c.has_tag_name("media-rep")).and_then(|m| m.attribute("src")))
            .unwrap_or_default()
            .to_string();
        let name = a.attribute("name").unwrap_or_default().to_string();
        assets.insert(id.to_string(), Asset { src, start: time_attr(a, "start"), name });
    }
    let sequence = root.descendants().find(|n| n.has_tag_name("sequence")).ok_or("no sequence in the file")?;
    let name = root
        .descendants()
        .find(|n| n.has_tag_name("project"))
        .and_then(|p| p.attribute("name"))
        .unwrap_or("Imported")
        .to_string();
    let spine = sequence.children().find(|c| c.has_tag_name("spine")).ok_or("sequence has no spine")?;
    let mut w = Walker { assets: &assets, lanes: Vec::new() };
    w.walk(spine, 0.0, time_attr(sequence, "tcStart"), "0", "");
    let mut lanes: Vec<Lane> = w.lanes.into_iter().map(|(_, l)| l).collect();
    for l in &mut lanes {
        l.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    }
    Ok(Arrangement { name, lanes })
}

/// Turns a `file://` URL into a plain path, undoing percent-encoding.
pub fn url_to_path(src: &str) -> String {
    let s = src.strip_prefix("file://localhost").or_else(|| src.strip_prefix("file://")).unwrap_or(src);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.10">
  <resources>
    <format id="r1" frameDuration="100/2400s"/>
    <asset id="r2" name="Vox" start="0s" duration="20s" hasAudio="1">
      <media-rep kind="original-media" src="file:///Users/me/Song/Media/Audio%20Files/Vox%231.wav"/>
    </asset>
    <asset id="r3" name="Bass" start="0s" duration="30s" hasAudio="1" src="file:///Users/me/Bass.wav"/>
  </resources>
  <library>
    <event name="Song">
      <project name="My Song">
        <sequence format="r1" tcStart="0s" duration="40s">
          <spine>
            <gap name="Gap" offset="0s" start="0s" duration="40s">
              <asset-clip ref="r2" lane="1" offset="4s" start="2s" duration="10s" audioRole="dialogue.Lead Vocal">
                <adjust-volume amount="-6dB">
                  <param name="amount">
                    <fadeIn type="easeIn" duration="1/2s"/>
                    <fadeOut type="easeOut" duration="1s"/>
                  </param>
                </adjust-volume>
              </asset-clip>
              <asset-clip ref="r3" lane="2" offset="0s" duration="30s" audioRole="music.Bass"/>
              <asset-clip ref="r2" lane="1" offset="20s" start="12s" duration="5s" audioRole="dialogue.Lead Vocal"/>
            </gap>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

    #[test]
    fn times() {
        assert_eq!(parse_time("1001/30000s"), Some(1001.0 / 30000.0));
        assert_eq!(parse_time("5s"), Some(5.0));
        assert_eq!(parse_time("0s"), Some(0.0));
        assert_eq!(parse_time("x"), None);
    }

    #[test]
    fn parses_project_export() {
        let a = parse(SAMPLE).unwrap();
        assert_eq!(a.name, "My Song");
        assert_eq!(a.lanes.len(), 2);
        let vox = &a.lanes[0];
        assert_eq!(vox.name, "Lead Vocal");
        assert_eq!(vox.clips.len(), 2);
        let c = &vox.clips[0];
        assert_eq!((c.start, c.offset, c.duration, c.gain_db), (4.0, 2.0, 10.0, -6.0));
        assert_eq!((c.fade_in, c.fade_out), (0.5, 1.0));
        assert_eq!(vox.clips[1].start, 20.0);
        assert_eq!(url_to_path(&c.src), "/Users/me/Song/Media/Audio Files/Vox#1.wav");
        assert_eq!(a.lanes[1].name, "Bass");
        assert_eq!(a.lanes[1].clips[0].src, "file:///Users/me/Bass.wav");
    }

    #[test]
    fn rejects_other_xml() {
        assert!(parse("<plist/>").is_err());
        assert!(parse("not xml").is_err());
    }
}
