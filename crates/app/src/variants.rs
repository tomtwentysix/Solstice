use solstice_model::{Id, Project, TrackKind};
use solstice_vst3::ClassInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Channels {
    Mono,
    Stereo,
    LeftRight,
    MidSide,
}

impl Channels {
    pub fn label(self) -> &'static str {
        match self {
            Channels::Mono => "Mono",
            Channels::Stereo => "Stereo",
            Channels::LeftRight => "Left/Right",
            Channels::MidSide => "Mid/Side",
        }
    }

    pub fn is_stereo(self) -> bool {
        self != Channels::Mono
    }
}

/// How one class differs from its siblings: plug-in suites such as LSP ship a class per channel
/// layout and band count ("Parametric Equalizer x16 Stereo").
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub family: String,
    pub channels: Option<Channels>,
    pub bands: Option<u32>,
}

pub fn parse(info: &ClassInfo) -> Variant {
    let mut channels = None;
    let mut bands = None;
    let mut rest = Vec::new();
    for word in info.name.split_whitespace() {
        let ch = match word {
            "Mono" => Some(Channels::Mono),
            "Stereo" => Some(Channels::Stereo),
            "LeftRight" => Some(Channels::LeftRight),
            "MidSide" => Some(Channels::MidSide),
            _ => None,
        };
        let size = word.strip_prefix('x').and_then(|n| n.parse::<u32>().ok());
        match (ch, size) {
            (Some(c), _) if channels.is_none() => channels = Some(c),
            (_, Some(n)) if bands.is_none() => bands = Some(n),
            _ => rest.push(word),
        }
    }
    // Fall back to the declared subcategory when the name doesn't say.
    if channels.is_none() {
        let sub = &info.subcategories;
        if sub.split('|').any(|c| c == "Mono") {
            channels = Some(Channels::Mono);
        } else if sub.split('|').any(|c| c == "Stereo") {
            channels = Some(Channels::Stereo);
        }
    }
    Variant { family: rest.join(" "), channels, bands }
}

/// Classes from the same module and family, including `info` itself.
pub fn members<'a>(catalog: &'a [ClassInfo], info: &ClassInfo) -> Vec<(&'a ClassInfo, Variant)> {
    let family = parse(info).family;
    catalog
        .iter()
        .filter(|c| c.path == info.path && c.is_instrument() == info.is_instrument())
        .map(|c| (c, parse(c)))
        .filter(|(_, v)| v.family == family)
        .collect()
}

/// Whether a family can go into a slot at all, and if so whether only by turning a mono strip
/// stereo (Logic's "Mono → Stereo").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    Fits,
    MonoToStereo,
    No,
}

pub fn fit(members: &[(&ClassInfo, Variant)], mono_slot: bool) -> Fit {
    let has = |f: &dyn Fn(Option<Channels>) -> bool| members.iter().any(|(_, v)| f(v.channels));
    let untagged = has(&|c| c.is_none());
    if mono_slot {
        if untagged || has(&|c| c == Some(Channels::Mono)) {
            Fit::Fits
        } else {
            Fit::MonoToStereo
        }
    } else if untagged || has(&|c| c.is_some_and(Channels::is_stereo)) {
        Fit::Fits
    } else {
        Fit::No
    }
}

/// The class to load for a slot: matching channel layout (plain Stereo before L/R or M/S),
/// and the given band count or else the smallest.
pub fn choose<'a>(members: &[(&'a ClassInfo, Variant)], mono_slot: bool, bands: Option<u32>) -> Option<&'a ClassInfo> {
    // Lower is better; mono plug-ins can't go on a stereo signal at all.
    let rank = |c: Option<Channels>| match (mono_slot, c) {
        (true, Some(Channels::Mono)) | (false, Some(Channels::Stereo)) => 0,
        (_, None) => 1,
        (true, Some(Channels::Stereo)) | (false, Some(_)) if c != Some(Channels::Mono) => 2,
        (true, Some(_)) => 3,
        _ => 9,
    };
    members
        .iter()
        .filter(|(_, v)| rank(v.channels) < 9)
        .min_by_key(|(_, v)| {
            let band_miss = match (bands, v.bands) {
                (Some(want), Some(have)) => (want != have) as u32 * 1000 + have,
                (_, Some(have)) => have,
                (_, None) => 0,
            };
            (rank(v.channels), band_miss)
        })
        .map(|(c, _)| *c)
}

/// Whether the signal reaching insert `index` of `owner` is still mono: a mono audio track
/// whose earlier inserts are all mono plug-ins.
pub fn slot_is_mono(p: &Project, catalog: &[ClassInfo], owner: Option<Id>, index: usize) -> bool {
    let Some(t) = owner.and_then(|id| p.track(id)) else { return false };
    if t.kind != TrackKind::Audio || t.stereo {
        return false;
    }
    t.channel.inserts.iter().take(index).all(|s| !slot_is_stereo(catalog, &s.uid))
}

/// A slot's plug-in outputs stereo when its class says so.
pub fn slot_is_stereo(catalog: &[ClassInfo], uid: &str) -> bool {
    catalog.iter().find(|c| c.uid == uid).and_then(|c| parse(c).channels).is_some_and(Channels::is_stereo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, sub: &str) -> ClassInfo {
        ClassInfo { uid: name.into(), name: name.into(), vendor: "LSP".into(), subcategories: sub.into(), path: "/x".into() }
    }

    fn catalog() -> Vec<ClassInfo> {
        let mut v = Vec::new();
        for n in [8, 16, 32] {
            for c in ["Mono", "Stereo", "LeftRight", "MidSide"] {
                let sub = if c == "Mono" { "Fx|EQ|Mono" } else { "Fx|EQ|Stereo" };
                v.push(info(&format!("Parametric Equalizer x{n} {c}"), sub));
            }
        }
        v.push(info("Multiband Compressor Mono x8", "Fx|Dynamics|Mono"));
        v.push(info("Multiband Compressor Stereo x8", "Fx|Dynamics|Stereo"));
        v.push(info("Hall Reverb", "Fx|Reverb|Stereo"));
        v
    }

    #[test]
    fn parses_variants() {
        let v = parse(&info("Parametric Equalizer x16 MidSide", "Fx|EQ|Stereo"));
        assert_eq!(v, Variant { family: "Parametric Equalizer".into(), channels: Some(Channels::MidSide), bands: Some(16) });
        let v = parse(&info("Multiband Compressor Mono x8", ""));
        assert_eq!((v.family.as_str(), v.channels, v.bands), ("Multiband Compressor", Some(Channels::Mono), Some(8)));
    }

    #[test]
    fn chooses_for_the_slot() {
        let cat = catalog();
        let peq = members(&cat, &cat[0]);
        assert_eq!(peq.len(), 12);
        assert_eq!(choose(&peq, true, None).unwrap().name, "Parametric Equalizer x8 Mono");
        assert_eq!(choose(&peq, false, None).unwrap().name, "Parametric Equalizer x8 Stereo");
        assert_eq!(choose(&peq, false, Some(32)).unwrap().name, "Parametric Equalizer x32 Stereo");
        let reverb = members(&cat, &cat[14]);
        assert_eq!(fit(&reverb, true), Fit::MonoToStereo);
        assert_eq!(fit(&reverb, false), Fit::Fits);
        assert_eq!(choose(&reverb, true, None).unwrap().name, "Hall Reverb");
    }

    #[test]
    fn mono_strip_turns_stereo_after_a_stereo_insert() {
        let cat = catalog();
        let mut p = Project::new("t", 48000);
        let t = p.add_track(TrackKind::Audio);
        let slot = |uid: &str| solstice_model::PluginSlot {
            id: 0,
            uid: uid.into(),
            path: "/x".into(),
            name: uid.into(),
            bypass: false,
            state: vec![],
            sidechain: None,
        };
        p.track_mut(t).unwrap().channel.inserts = vec![slot("Parametric Equalizer x8 Mono"), slot("Hall Reverb")];
        assert!(slot_is_mono(&p, &cat, Some(t), 0));
        assert!(slot_is_mono(&p, &cat, Some(t), 1));
        assert!(!slot_is_mono(&p, &cat, Some(t), 2));
        assert!(!slot_is_mono(&p, &cat, None, 0));
    }
}
