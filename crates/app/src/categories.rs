use serde::Deserialize;
use solstice_model::StripSection;
use solstice_vst3::ClassInfo;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

/// Categories a plug-in can be given beyond the VST3 subcategories it declares.
pub const GUITAR: [&str; 3] = ["Amp", "Cabinet", "Pedal"];

/// Insert pictures by category; plug-ins in none of these get the generic one.
const PICTURES: [(&str, &str); 14] = [
    ("Amp", "amp"),
    ("Cabinet", "cabinet"),
    ("Pedal", "pedal"),
    ("Dynamics", "dynamics"),
    ("EQ", "eq"),
    ("Filter", "filter"),
    ("Delay", "delay"),
    ("Reverb", "reverb"),
    ("Distortion", "distortion"),
    ("Modulation", "modulation"),
    ("Pitch Shift", "pitch"),
    ("Analyzer", "analyzer"),
    ("Spatial", "spatial"),
    ("Surround", "spatial"),
];

/// The icon drawn on an insert: the picture for the category its section stands for when the
/// plug-in has it (a pedal in Pedals even if it is also an amp), else for its first category
/// that has one.
pub fn picture(cats: &[String], section: Option<StripSection>) -> String {
    let prefer = section.and_then(|s| match s {
        StripSection::Pedals => Some("Pedal"),
        s => s.requires(),
    });
    let has = |c: &str| cats.iter().any(|x| x.eq_ignore_ascii_case(c));
    let name = prefer
        .filter(|p| has(p))
        .and_then(|p| PICTURES.iter().find(|(c, _)| *c == p))
        .or_else(|| cats.iter().find_map(|c| PICTURES.iter().find(|(p, _)| p.eq_ignore_ascii_case(c))))
        .map_or("other", |(_, n)| n);
    format!("solstice-strip-{name}-symbolic")
}

/// The picture an empty section shows in place of its first insert.
pub fn section_picture(section: StripSection) -> String {
    let name = match section {
        StripSection::Pedals => "pedal",
        StripSection::Amp => "amp",
        StripSection::Cabinet => "cabinet",
        StripSection::Effects => "other",
    };
    format!("solstice-strip-{name}-symbolic")
}

/// `data/plugin-categories.json`: categories by plug-in ID, and by vendor and name for plug-ins
/// whose IDs differ between formats. Names match the class name or its family, ignoring case.
#[derive(Deserialize, Default)]
struct Table {
    ids: HashMap<String, Vec<String>>,
    names: Vec<Named>,
}

#[derive(Deserialize)]
struct Named {
    vendor: String,
    name: String,
    categories: Vec<String>,
}

static TABLE: LazyLock<Table> =
    LazyLock::new(|| serde_json::from_str(include_str!("../../../data/plugin-categories.json")).unwrap_or_default());

/// User choices (from settings) first, then the shipped table, then the plug-in's own
/// subcategories.
pub fn of(info: &ClassInfo, user: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    resolve(info, user, &TABLE)
}

fn resolve(info: &ClassInfo, user: &BTreeMap<String, Vec<String>>, table: &Table) -> Vec<String> {
    if let Some(c) = user.get(&info.uid).filter(|c| !c.is_empty()) {
        return c.clone();
    }
    if let Some(c) = table.ids.get(&info.uid) {
        return c.clone();
    }
    let vendor = info.vendor.to_lowercase();
    let names = names(info);
    if let Some(n) = table.names.iter().find(|n| n.vendor.to_lowercase() == vendor && names.contains(&n.name.to_lowercase())) {
        return n.categories.clone();
    }
    declared(info)
}

/// The class name, its family without the channel and band words, and the family without a
/// leading vendor word ("LSP Impulse Responses Mono" also matches "Impulse Responses").
fn names(info: &ClassInfo) -> Vec<String> {
    let family = crate::variants::parse(info).family.to_lowercase();
    let maker = info.vendor.split_whitespace().next().unwrap_or("").to_lowercase();
    let mut v = vec![info.name.to_lowercase()];
    if !maker.is_empty()
        && let Some(rest) = family.strip_prefix(&maker).and_then(|r| r.strip_prefix(' '))
    {
        v.push(rest.to_string());
    }
    v.push(family);
    v
}

/// The specific VST3 subcategories, skipping the generic kind and channel-format tags
/// ("Fx|Dynamics|Stereo" is Dynamics).
fn declared(info: &ClassInfo) -> Vec<String> {
    const GENERIC: [&str; 6] = ["Fx", "Instrument", "Stereo", "Mono", "Surround", "Ambisonics"];
    let mut v: Vec<String> = Vec::new();
    for c in info.subcategories.split('|').map(str::trim).filter(|c| !c.is_empty() && !GENERIC.contains(c)) {
        if !v.iter().any(|x| x == c) {
            v.push(c.to_string());
        }
    }
    if v.is_empty() {
        v.push(if info.is_instrument() { "Instrument".into() } else { "Other".into() });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(uid: &str, vendor: &str, name: &str, sub: &str) -> ClassInfo {
        ClassInfo { uid: uid.into(), name: name.into(), vendor: vendor.into(), subcategories: sub.into(), path: Default::default() }
    }

    fn table() -> Table {
        serde_json::from_str(
            r#"{"ids":{"x:amp":["Amp","Pedal"]},
                "names":[{"vendor":"Ignite Amps","name":"Emissary","categories":["Amp"]},
                         {"vendor":"LSP","name":"Impulse Responses","categories":["Cabinet","Reverb"]}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn user_choice_then_table_id_then_name_then_declared() {
        let t = table();
        let mut user = BTreeMap::new();
        let by_id = info("x:amp", "Someone", "Whatever", "Fx|Distortion");
        assert_eq!(resolve(&by_id, &user, &t), ["Amp", "Pedal"]);
        user.insert("x:amp".to_string(), vec!["Reverb".to_string()]);
        assert_eq!(resolve(&by_id, &user, &t), ["Reverb"]);
        // An empty user choice means none was made.
        user.insert("x:amp".to_string(), vec![]);
        assert_eq!(resolve(&by_id, &user, &t), ["Amp", "Pedal"]);

        let by_name = info("vst3:1234", "ignite amps", "EMISSARY", "Fx|Distortion|Stereo");
        assert_eq!(resolve(&by_name, &user, &t), ["Amp"]);
        let other_vendor = info("vst3:5678", "Someone Else", "Emissary", "Fx|Distortion|Stereo");
        assert_eq!(resolve(&other_vendor, &user, &t), ["Distortion"]);
        let family = info("lv2:ir", "LSP", "LSP Impulse Responses Mono", "Fx|Reverb|Mono");
        assert_eq!(resolve(&family, &user, &t), ["Cabinet", "Reverb"]);
    }

    #[test]
    fn declared_subcategories() {
        let none = BTreeMap::new();
        let t = Table::default();
        let cats = |sub: &str| resolve(&info("", "", "", sub), &none, &t);
        assert_eq!(cats("Fx|Dynamics|Stereo"), ["Dynamics"]);
        assert_eq!(cats("Fx|EQ|Filter|EQ"), ["EQ", "Filter"]);
        assert_eq!(cats("Instrument|Instrument|Sampler|Instrument|Drum|Mono"), ["Sampler", "Drum"]);
        assert_eq!(cats("Fx"), ["Other"]);
        assert_eq!(cats("Instrument"), ["Instrument"]);
    }

    #[test]
    fn pictures_follow_the_section_then_the_categories() {
        let cats = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let nam = cats(&["Amp", "Pedal"]);
        assert_eq!(picture(&nam, Some(StripSection::Amp)), "solstice-strip-amp-symbolic");
        assert_eq!(picture(&nam, Some(StripSection::Pedals)), "solstice-strip-pedal-symbolic");
        assert_eq!(picture(&nam, None), "solstice-strip-amp-symbolic");
        let comp = cats(&["Dynamics"]);
        assert_eq!(picture(&comp, Some(StripSection::Pedals)), "solstice-strip-dynamics-symbolic");
        assert_eq!(picture(&cats(&["Tools", "pitch shift"]), None), "solstice-strip-pitch-symbolic");
        assert_eq!(picture(&cats(&["Other"]), Some(StripSection::Effects)), "solstice-strip-other-symbolic");
        assert_eq!(picture(&[], None), "solstice-strip-other-symbolic");
        // Every picture is bundled.
        let resources = include_str!("../resources.gresource.xml");
        for (_, name) in PICTURES.iter().chain([&("", "other")]) {
            assert!(resources.contains(&format!("solstice-strip-{name}-symbolic.svg")), "{name}");
        }
    }

    #[test]
    fn shipped_table_loads_and_covers_the_guitar_built_ins() {
        let none = BTreeMap::new();
        assert!(!TABLE.ids.is_empty() && !TABLE.names.is_empty());
        let nam = info("solstice:nam", "SolarFlare Developments", "Neural Amp Modeler", "Fx");
        assert_eq!(of(&nam, &none), ["Amp", "Pedal"]);
        let ir = info("solstice:ir", "SolarFlare Developments", "Impulse Response", "Fx");
        assert_eq!(of(&ir, &none), ["Cabinet", "Reverb"]);
        for (id, cats) in &TABLE.ids {
            assert!(solstice_dsp::spec(id).is_some(), "{id}");
            assert!(!cats.is_empty(), "{id}");
        }
    }
}
