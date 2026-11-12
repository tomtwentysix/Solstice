//! Logic's Musical Typing: while it is on, the letter rows of the computer keyboard play the
//! selected instrument, and those keys stop working as shortcuts.

use crate::studio::Ctx;
use adw::prelude::*;
use gtk::{gdk, glib};
use solstice_engine::Command;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Keys from C upwards, Logic's layout: the home row is white keys, the row above black keys.
const KEYS: [(char, u8); 18] = [
    ('a', 0), ('w', 1), ('s', 2), ('e', 3), ('d', 4), ('f', 5), ('t', 6), ('g', 7), ('y', 8),
    ('h', 9), ('u', 10), ('j', 11), ('k', 12), ('o', 13), ('l', 14), ('p', 15), (';', 16), ('\'', 17),
];

struct State {
    window: RefCell<Option<adw::Window>>,
    /// The note at the 'A' key.
    base: Cell<u8>,
    velocity: Cell<u8>,
    held: RefCell<HashMap<char, u8>>,
    status: RefCell<Option<gtk::Label>>,
}

thread_local! {
    static STATE: State = State {
        window: RefCell::default(),
        base: Cell::new(60),
        velocity: Cell::new(100),
        held: RefCell::default(),
        status: RefCell::default(),
    };
}

pub fn is_on() -> bool {
    STATE.with(|s| s.window.borrow().is_some())
}

fn key_char(key: gdk::Key) -> Option<char> {
    key.to_lower().to_unicode()
}

fn update_status() {
    STATE.with(|s| {
        if let Some(l) = &*s.status.borrow() {
            let octave = s.base.get() as i32 / 12 - 1;
            l.set_text(&format!("Octave C{octave} (Z, X)   Velocity {} (C, V)", s.velocity.get()));
        }
    });
}

/// Plays or handles a pressed key. Returns false for keys Musical Typing doesn't use.
pub fn press(ctx: &Ctx, key: gdk::Key) -> bool {
    key_char(key).is_some_and(|c| press_char(ctx, c))
}

fn press_char(ctx: &Ctx, c: char) -> bool {
    STATE.with(|s| {
        match c {
            'z' => s.base.set(s.base.get().saturating_sub(12).max(12)),
            'x' => s.base.set((s.base.get() + 12).min(108)),
            'c' => s.velocity.set(s.velocity.get().saturating_sub(20).max(1)),
            'v' => s.velocity.set((s.velocity.get() + 20).min(127)),
            _ => {
                let Some(&(_, offset)) = KEYS.iter().find(|k| k.0 == c) else { return false };
                // Held keys auto-repeat; only the first press plays.
                if s.held.borrow().contains_key(&c) {
                    return true;
                }
                let note = (s.base.get() + offset).min(127);
                s.held.borrow_mut().insert(c, note);
                ctx.send(Command::Midi([0x90, note, s.velocity.get()]));
                return true;
            }
        }
        update_status();
        true
    })
}

pub fn release(ctx: &Ctx, key: gdk::Key) {
    if let Some(c) = key_char(key) {
        release_char(ctx, c);
    }
}

fn release_char(ctx: &Ctx, c: char) {
    if let Some(note) = STATE.with(|s| s.held.borrow_mut().remove(&c)) {
        ctx.send(Command::Midi([0x80, note, 0]));
    }
}

fn release_all(ctx: &Ctx) {
    let notes: Vec<u8> = STATE.with(|s| s.held.borrow_mut().drain().map(|(_, n)| n).collect());
    for n in notes {
        ctx.send(Command::Midi([0x80, n, 0]));
    }
}

/// Shows or hides the Musical Typing window.
pub fn toggle(ctx: &Ctx, parent: &gtk::Window) {
    if let Some(w) = STATE.with(|s| s.window.borrow().clone()) {
        w.close();
        return;
    }
    let win = adw::Window::builder()
        .title("Musical Typing")
        .transient_for(parent)
        .resizable(false)
        .hide_on_close(false)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_bottom(12);
    let black = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let white = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    black.set_margin_start(22);
    // Gaps in the black row where a piano has none: after E and after B.
    let gaps = [4, 11, 16];
    for &(c, offset) in &KEYS {
        let is_black = matches!(offset % 12, 1 | 3 | 6 | 8 | 10);
        let b = gtk::Button::with_label(&c.to_uppercase().to_string());
        b.set_size_request(40, 40);
        b.set_focusable(false);
        if is_black {
            b.add_css_class("osd");
        }
        let click = gtk::GestureClick::new();
        let (ctx2, ctx3) = (ctx.clone(), ctx.clone());
        click.connect_pressed(move |_, _, _, _| {
            press_char(&ctx2, c);
        });
        click.connect_released(move |_, _, _, _| release_char(&ctx3, c));
        b.add_controller(click);
        if is_black {
            black.append(&b);
        } else {
            white.append(&b);
            if gaps.contains(&offset) {
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_size_request(40, 0);
                black.append(&spacer);
            }
        }
    }
    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    body.append(&black);
    body.append(&white);
    body.append(&status);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&body));
    win.set_content(Some(&view));

    let keys = gtk::EventControllerKey::new();
    let (ctx2, ctx3) = (ctx.clone(), ctx.clone());
    keys.connect_key_pressed(move |_, key, _, mods| {
        if mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) || !press(&ctx2, key) {
            return glib::Propagation::Proceed;
        }
        glib::Propagation::Stop
    });
    keys.connect_key_released(move |_, key, _, _| release(&ctx3, key));
    win.add_controller(keys);
    {
        let ctx = ctx.clone();
        win.connect_close_request(move |_| {
            release_all(&ctx);
            STATE.with(|s| {
                s.window.replace(None);
                s.status.replace(None);
            });
            glib::Propagation::Proceed
        });
    }
    STATE.with(|s| {
        s.window.replace(Some(win.clone()));
        s.status.replace(Some(status));
    });
    update_status();
    win.present();
}

/// Lets the main window's keys play while Musical Typing is open.
pub fn attach(ctx: &Ctx, win: &impl IsA<gtk::Widget>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (ctx2, ctx3) = (ctx.clone(), ctx.clone());
    keys.connect_key_pressed(move |c, key, _, mods| {
        let root = c.widget().and_then(|w| w.root());
        let typing_in_text = root.as_ref().and_then(|r| r.focus()).is_some_and(|f| f.is::<gtk::Text>());
        let dialog_open = root.and_downcast::<adw::ApplicationWindow>().is_some_and(|w| w.visible_dialog().is_some());
        if !is_on() || typing_in_text || dialog_open || mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        if press(&ctx2, key) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
    });
    keys.connect_key_released(move |_, key, _, _| release(&ctx3, key));
    win.add_controller(keys);
}
