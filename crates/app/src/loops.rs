//! A loop browser over the user's own loop folders, like Logic's Loop Browser: tempo and key
//! come from a WAV's ACID chunk or the file name ("Funk Drums 96bpm Am.wav").

use crate::import::is_audio;
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub struct Loop {
    pub path: PathBuf,
    pub name: String,
    pub tempo: Option<f64>,
    pub key: Option<String>,
}

const NOTES: [&str; 12] = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];

/// A tempo such as "96bpm", "96 BPM" or a bare 60..200 number between separators.
pub fn tempo_from_name(name: &str) -> Option<f64> {
    let lower = Path::new(name).file_stem().map_or(name.to_string(), |s| s.to_string_lossy().into_owned()).to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '.').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(n) = w.strip_suffix("bpm").and_then(|n| n.parse::<f64>().ok()) {
            return Some(n);
        }
        if *w == "bpm"
            && let Some(n) = i.checked_sub(1).and_then(|j| words[j].parse::<f64>().ok())
        {
            return Some(n);
        }
    }
    words.iter().filter_map(|w| w.parse::<f64>().ok()).find(|n| (60.0..=200.0).contains(n) && n.fract() == 0.0)
}

/// A key such as "Am", "C#min", "Eb major" or "F# minor" in a file name.
pub fn key_from_name(name: &str) -> Option<String> {
    let stem = Path::new(name).file_stem().map_or(name.to_string(), |s| s.to_string_lossy().into_owned());
    let words: Vec<&str> = stem.split(|c: char| c == ' ' || c == '_' || c == '-' || c == '.').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        let mut chars = w.chars();
        let Some(letter) = chars.next().filter(|c| ('A'..='G').contains(c)) else { continue };
        let rest: String = chars.collect();
        let (accidental, quality) = match rest.chars().next() {
            Some('#' | '♯') => (1, &rest[rest.char_indices().nth(1).map_or(rest.len(), |x| x.0)..]),
            Some('b' | '♭') if rest.len() > 1 || words.get(i + 1).is_some() => (-1, &rest[rest.char_indices().nth(1).map_or(rest.len(), |x| x.0)..]),
            _ => (0, rest.as_str()),
        };
        let quality = if quality.is_empty() { words.get(i + 1).copied().unwrap_or("") } else { quality };
        let minor = match quality.to_lowercase().as_str() {
            "m" | "min" | "minor" => true,
            "maj" | "major" | "" => false,
            _ => continue,
        };
        // A lone capital letter is too likely to be a word ("A Loop.wav") unless a quality follows.
        if w.len() == 1 && words.get(i + 1).is_none_or(|q| !matches!(q.to_lowercase().as_str(), "major" | "minor" | "maj" | "min" | "m")) {
            continue;
        }
        let base = match letter {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            _ => 11,
        };
        let note = NOTES[(base + accidental + 12) as usize % 12];
        return Some(format!("{note} {}", if minor { "minor" } else { "major" }));
    }
    None
}

/// The tempo in a WAV's ACID chunk, which loop libraries use to tag their files.
pub fn acid_tempo(path: &Path) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    // Chunks are id, little-endian size, then data padded to an even length.
    for _ in 0..64 {
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch).ok()?;
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[0..4] == b"acid" && size >= 24 {
            let mut data = [0u8; 24];
            f.read_exact(&mut data).ok()?;
            let tempo = f32::from_le_bytes([data[20], data[21], data[22], data[23]]) as f64;
            return (20.0..=400.0).contains(&tempo).then_some(tempo);
        }
        f.seek(SeekFrom::Current((size + size % 2) as i64)).ok()?;
    }
    None
}

pub fn scan(folders: &[PathBuf]) -> Vec<Loop> {
    fn walk(dir: &Path, out: &mut Vec<Loop>, depth: u32) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            if p.is_dir() && depth < 6 {
                walk(&p, out, depth + 1);
            } else if is_audio(&p) {
                let name = p.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                let file = p.file_name().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                let tempo = acid_tempo(&p).or_else(|| tempo_from_name(&file));
                out.push(Loop { key: key_from_name(&file), tempo, name, path: p });
            }
        }
    }
    let mut out = Vec::new();
    for f in folders {
        walk(f, &mut out, 0);
    }
    out.sort_by_key(|l| l.name.to_lowercase());
    out
}

pub fn build(ctx: &Ctx) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.set_margin_start(8);
    col.set_margin_end(8);
    col.set_margin_top(8);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let search = gtk::SearchEntry::new();
    search.set_hexpand(true);
    search.set_placeholder_text(Some("Search Loops"));
    let add = gtk::Button::from_icon_name("folder-new-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("Add Loop Folder…"));
    top.append(&search);
    top.append(&add);
    col.append(&top);
    let tempo_only = gtk::CheckButton::with_label("Near the project tempo");
    col.append(&tempo_only);
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let scroller = gtk::ScrolledWindow::builder().child(&list).vexpand(true).build();
    col.append(&scroller);
    let empty = gtk::Label::new(Some("Add a folder of loops to browse them here"));
    empty.add_css_class("dim-label");
    empty.set_wrap(true);
    list.set_placeholder(Some(&empty));

    let loops: Rc<RefCell<Vec<Loop>>> = Rc::default();
    let fill: Rc<dyn Fn()> = {
        let (ctx, loops, list, search, tempo_only) = (ctx.clone(), loops.clone(), list.clone(), search.clone(), tempo_only.clone());
        Rc::new(move || {
            list.remove_all();
            let q = search.text().to_lowercase();
            let project_tempo = ctx.s().project.tempo;
            for l in loops.borrow().iter() {
                let near = l.tempo.is_some_and(|t| (t / project_tempo - 1.0).abs() < 0.05);
                if (!q.is_empty() && !l.name.to_lowercase().contains(&q)) || (tempo_only.is_active() && !near) {
                    continue;
                }
                let info: Vec<String> = l.tempo.map(|t| format!("{t:.0} BPM")).into_iter().chain(l.key.clone()).collect();
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&l.name))
                    .subtitle(info.join(" · "))
                    .activatable(true)
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("media-playlist-repeat-symbolic"));
                let src = gtk::DragSource::new();
                src.set_actions(gdk::DragAction::COPY);
                let file = gio::File::for_path(&l.path);
                src.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&[file.clone()]).to_value())));
                row.add_controller(src);
                row.set_widget_name(&l.path.to_string_lossy());
                list.append(&row);
            }
        })
    };
    let rescan: Rc<dyn Fn()> = {
        let (ctx, loops, fill) = (ctx.clone(), loops.clone(), fill.clone());
        Rc::new(move || {
            let folders = ctx.s().settings.loop_folders.clone();
            let (loops, fill) = (loops.clone(), fill.clone());
            glib::spawn_future_local(async move {
                let found = gio::spawn_blocking(move || scan(&folders)).await.unwrap_or_default();
                *loops.borrow_mut() = found;
                fill();
            });
        })
    };
    {
        let fill = fill.clone();
        search.connect_search_changed(move |_| fill());
    }
    {
        let fill = fill.clone();
        tempo_only.connect_toggled(move |_| fill());
    }
    {
        let (ctx, rescan) = (ctx.clone(), rescan.clone());
        add.connect_clicked(move |b| {
            let (ctx, rescan) = (ctx.clone(), rescan.clone());
            let chooser = gtk::FileDialog::builder().title("Add Loop Folder").build();
            let win = b.root().and_downcast::<gtk::Window>();
            chooser.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(path) = f.path()
                {
                    {
                        let mut s = ctx.m();
                        if !s.settings.loop_folders.contains(&path) {
                            s.settings.loop_folders.push(path);
                            s.settings.save();
                        }
                    }
                    rescan();
                }
            });
        });
    }
    // The folders being browsed, each removable.
    let folders = gtk::MenuButton::builder().icon_name("view-list-symbolic").tooltip_text("Loop Folders").build();
    {
        let (ctx, rescan) = (ctx.clone(), rescan.clone());
        folders.set_create_popup_func(move |mb| {
            let pop = gtk::Popover::new();
            let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let dirs = ctx.s().settings.loop_folders.clone();
            if dirs.is_empty() {
                col.append(&gtk::Label::new(Some("No loop folders yet")));
            }
            for dir in dirs {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let name = gtk::Label::new(Some(&dir.display().to_string()));
                name.set_hexpand(true);
                name.set_xalign(0.0);
                name.set_ellipsize(gtk::pango::EllipsizeMode::Start);
                name.set_max_width_chars(36);
                row.append(&name);
                let remove = gtk::Button::from_icon_name("user-trash-symbolic");
                remove.add_css_class("flat");
                remove.set_tooltip_text(Some("Remove Folder"));
                let (ctx, rescan, pop) = (ctx.clone(), rescan.clone(), pop.clone());
                remove.connect_clicked(move |_| {
                    {
                        let mut s = ctx.m();
                        s.settings.loop_folders.retain(|d| *d != dir);
                        s.settings.save();
                    }
                    pop.popdown();
                    rescan();
                });
                row.append(&remove);
                col.append(&row);
            }
            pop.set_child(Some(&col));
            mb.set_popover(Some(&pop));
        });
    }
    top.append(&folders);
    {
        // "Near the project tempo" follows tempo changes.
        let (ctx2, fill) = (ctx.clone(), fill.clone());
        let last = std::cell::Cell::new(ctx.s().project.tempo);
        ctx.on(move |c| {
            if c == Change::Project {
                let tempo = ctx2.s().project.tempo;
                if tempo != last.get() {
                    last.set(tempo);
                    fill();
                }
            }
        });
    }
    {
        let (ctx, loops) = (ctx.clone(), loops.clone());
        list.connect_row_activated(move |_, row| {
            let path = PathBuf::from(row.widget_name().as_str());
            let tempo = loops.borrow().iter().find(|l| l.path == path).and_then(|l| l.tempo);
            place(&ctx, &path, tempo);
        });
    }
    rescan();
    col.upcast()
}

/// Imports a loop at the playhead. An empty project takes on the loop's tempo, as Logic
/// offers; otherwise a different tempo is pointed out, since loops aren't stretched yet.
fn place(ctx: &Ctx, path: &Path, tempo: Option<f64>) {
    let empty = ctx.s().project.tracks.iter().all(|t| t.regions.is_empty());
    if let Some(t) = tempo {
        let current = ctx.s().project.tempo;
        if empty && (t - current).abs() > 0.01 {
            ctx.edit("Adopt Loop Tempo", |p| {
                let old = p.time();
                p.tempo = t;
                p.retime(&old);
            });
            ctx.message(&format!("The project tempo is now {t:.0} BPM to match the loop"));
        } else if (t / current - 1.0).abs() > 0.01 {
            ctx.message(&format!("This loop is {t:.0} BPM and the project is {current:.0} BPM; it plays at its own tempo"));
        }
    }
    let row = ctx.s().selected_track.and_then(|id| ctx.s().project.tracks.iter().position(|t| t.id == id));
    crate::timeline::import_files(ctx, &[path.to_path_buf()], row, ctx.position());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_and_key_from_names() {
        assert_eq!(tempo_from_name("Funk Drums 96bpm Am.wav"), Some(96.0));
        assert_eq!(tempo_from_name("pad_120_BPM_Cmaj.wav"), Some(120.0));
        assert_eq!(tempo_from_name("Bass Line 01 128.wav"), Some(128.0));
        assert_eq!(tempo_from_name("Snare.wav"), None);
        assert_eq!(key_from_name("Funk Drums 96bpm Am.wav").as_deref(), Some("A minor"));
        assert_eq!(key_from_name("pad_120_BPM_C#min.wav").as_deref(), Some("C♯ minor"));
        assert_eq!(key_from_name("Lead Eb major.wav").as_deref(), Some("E♭ major"));
        assert_eq!(key_from_name("A Loop.wav"), None);
        assert_eq!(key_from_name("Drums.wav"), None);
    }

    #[test]
    fn reads_acid_tempo() {
        let dir = std::env::temp_dir().join(format!("solstice-acid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loop.wav");
        let mut acid = vec![0u8; 24];
        acid[20..24].copy_from_slice(&104.0f32.to_le_bytes());
        let mut body = b"WAVE".to_vec();
        body.extend(b"junk");
        body.extend(3u32.to_le_bytes());
        body.extend([0u8; 4]);
        body.extend(b"acid");
        body.extend(24u32.to_le_bytes());
        body.extend(&acid);
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);
        std::fs::write(&path, file).unwrap();
        assert_eq!(acid_tempo(&path), Some(104.0));
    }
}
