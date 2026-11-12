use crate::plugins;
use crate::variants;
use solstice_vst3::ClassInfo;
use crate::studio::Ctx;
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_model::*;

// Plug-in drag and drop payloads: "slot:<owner>:<slot id>", "inst:<track>:<slot id>" or
// "lib:<class uid>", where <owner> is a channel id or "m" for the stereo output.
pub fn owner_key(id: Option<Id>) -> String {
    id.map_or("m".into(), |i| i.to_string())
}

pub enum Source {
    Insert { owner: Option<Id>, slot: Id },
    Instrument { track: Id, slot: Id },
    Library(String),
}

pub fn parse_payload(s: &str) -> Option<Source> {
    let mut parts = s.splitn(3, ':');
    let kind = parts.next()?;
    match kind {
        "lib" => Some(Source::Library(parts.next()?.to_string())),
        "slot" | "inst" => {
            let owner = parts.next()?;
            let slot: Id = parts.next()?.parse().ok()?;
            let owner = if owner == "m" { None } else { Some(owner.parse().ok()?) };
            if kind == "slot" {
                Some(Source::Insert { owner, slot })
            } else {
                Some(Source::Instrument { track: owner?, slot })
            }
        }
        _ => None,
    }
}

pub fn drag_source(w: &impl IsA<gtk::Widget>, payload: String) {
    let src = gtk::DragSource::new();
    src.set_actions(gdk::DragAction::MOVE | gdk::DragAction::COPY);
    src.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
    w.add_controller(src);
}

/// Resolves a payload into a slot for the target. Moves take the slot out of its source so the
/// running instance and its settings come along; copies and library drops get a new slot.
/// `lib` is the library class already resolved for the target, since the studio is borrowed
/// for the edit while this runs.
pub fn take_for_drop(p: &mut Project, src: &Source, copy: bool, states: &Option<Vec<u8>>, lib: Option<&ClassInfo>) -> Option<PluginSlot> {
    match src {
        Source::Library(_) => Some(plugins::new_slot(p, lib?)),
        Source::Insert { owner, slot } => {
            let ch = p.channel_mut(*owner)?;
            let i = ch.inserts.iter().position(|s| s.id == *slot)?;
            if copy {
                let mut s = ch.inserts[i].clone();
                s.id = p.alloc_id();
                s.state = states.clone().unwrap_or_default();
                Some(s)
            } else {
                Some(ch.inserts.remove(i))
            }
        }
        Source::Instrument { track, slot } => {
            let t = p.track_mut(*track)?;
            if t.instrument.as_ref().is_none_or(|s| s.id != *slot) {
                return None;
            }
            if copy {
                let mut s = t.instrument.clone().unwrap();
                s.id = p.alloc_id();
                s.state = states.clone().unwrap_or_default();
                Some(s)
            } else {
                t.instrument.take()
            }
        }
    }
}

pub fn source_state(ctx: &Ctx, src: &Source) -> Option<Vec<u8>> {
    let slot = match src {
        Source::Insert { slot, .. } | Source::Instrument { slot, .. } => *slot,
        Source::Library(_) => return None,
    };
    ctx.s().plugins.get(&slot).map(|l| l.inst.state())
}

pub fn is_instrument(ctx: &Ctx, src: &Source) -> bool {
    match src {
        Source::Library(uid) => ctx.s().catalog.iter().any(|c| &c.uid == uid && c.is_instrument()),
        Source::Instrument { .. } => true,
        Source::Insert { .. } => false,
    }
}

/// The library class for a drop, resolved to the variant that suits the target slot.
pub fn library_class(ctx: &Ctx, src: &Source, mono_slot: bool) -> Option<ClassInfo> {
    let Source::Library(uid) = src else { return None };
    let info = ctx.s().catalog.iter().find(|c| &c.uid == uid).cloned()?;
    plugins::resolve(ctx, &info, mono_slot)
}

/// Drop onto insert position `index` of `owner`. Within one strip this reorders; from elsewhere
/// it replaces the plug-in at `index` (or appends when `index` is the empty slot). A mono
/// plug-in can't land where the signal is already stereo.
pub fn drop_insert(ctx: &Ctx, value: &glib::Value, owner: Option<Id>, index: usize, copy: bool) -> bool {
    let Ok(payload) = value.get::<String>() else { return false };
    let Some(src) = parse_payload(&payload) else { return false };
    let midi_fx = matches!(&src, Source::Library(uid)
        if ctx.s().catalog.iter().any(|c| &c.uid == uid && plugins::kind(c) == plugins::Kind::MidiFx));
    if midi_fx || is_instrument(ctx, &src) {
        return false;
    }
    let mono = {
        let s = ctx.s();
        variants::slot_is_mono(&s.project, &s.catalog, owner, index)
    };
    let lib = library_class(ctx, &src, mono);
    if matches!(src, Source::Library(_)) && lib.is_none() {
        return false;
    }
    if let Source::Insert { owner: o, slot } = &src {
        let s = ctx.s();
        let uid = s.project.channel(*o).and_then(|c| c.inserts.iter().find(|x| x.id == *slot)).map(|x| x.uid.clone());
        let is_mono = uid.and_then(|u| s.catalog.iter().find(|c| c.uid == u).cloned()).and_then(|c| variants::parse(&c).channels)
            == Some(variants::Channels::Mono);
        if is_mono && !mono {
            return false;
        }
    }
    let same_strip = matches!(src, Source::Insert { owner: o, .. } if o == owner);
    let states = if copy { source_state(ctx, &src) } else { None };
    let label = if copy { "Copy Plug-in" } else if matches!(src, Source::Library(_)) { "Add Plug-in" } else { "Move Plug-in" };
    ctx.edit(label, move |p| {
        let from = match &src {
            Source::Insert { owner: o, slot } if *o == owner => p.channel(owner).and_then(|c| c.inserts.iter().position(|s| s.id == *slot)),
            _ => None,
        };
        let Some(slot) = take_for_drop(p, &src, copy, &states, lib.as_ref()) else { return };
        let Some(ch) = p.channel_mut(owner) else { return };
        let mut at = index.min(ch.inserts.len());
        if same_strip && !copy {
            if let Some(f) = from
                && f < at
            {
                at -= 1;
            }
            ch.inserts.insert(at.min(ch.inserts.len()), slot);
        } else if at < ch.inserts.len() {
            ch.inserts[at] = slot;
        } else {
            ch.inserts.push(slot);
        }
    });
    true
}

pub fn drop_target(ctx: &Ctx, w: &impl IsA<gtk::Widget>, on_drop: impl Fn(&Ctx, &glib::Value, bool) -> bool + 'static) {
    let t = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE | gdk::DragAction::COPY);
    let ctx = ctx.clone();
    let w2 = w.clone().upcast::<gtk::Widget>();
    t.connect_enter(move |_, _, _| {
        w2.add_css_class("drop-hover");
        gdk::DragAction::MOVE
    });
    let w3 = w.clone().upcast::<gtk::Widget>();
    t.connect_leave(move |_| w3.remove_css_class("drop-hover"));
    t.connect_drop(move |t, value, _, _| {
        // Alt held at drop copies.
        let copy = t.current_event_state().contains(gdk::ModifierType::ALT_MASK);
        on_drop(&ctx, value, copy)
    });
    w.add_controller(t);
}
