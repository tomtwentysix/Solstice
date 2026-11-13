use serde::Deserialize;
use solstice_vst3::ClassInfo;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

/// Categories a plug-in can be given beyond the VST3 subcategories it declares.
pub const GUITAR: [&str; 3] = ["Amp", "Cabinet", "Pedal"];

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
    fn shipped_table_loads_and_covers_the_guitar_built_ins() {
        let none = BTreeMap::new();
        assert!(!TABLE.ids.is_empty() && !TABLE.names.is_empty());
        let nam = info("solstice:nam", "SolarFlare Developments", "Neural Amp Modeler", "Fx");
        assert_eq!(of(&nam, &none), ["Amp", "Pedal"]);
        let ir = info("solstice:ir", "SolarFlare Developments", "IR Loader", "Fx");
        assert_eq!(of(&ir, &none), ["Cabinet", "Reverb"]);
        for (id, cats) in &TABLE.ids {
            assert!(id.starts_with(solstice_dsp::PREFIX), "{id}");
            assert!(!cats.is_empty(), "{id}");
        }
    }
}
