use crate::canvas::{self, Canvas, rect, rgba, rounded};
use crate::studio::Ctx;
use adw::prelude::*;
use gtk::{glib, graphene, pango};
use rtrb::Consumer;
use solstice_dsp::pitch::{self, Detector};
use solstice_engine::Handle;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering::Relaxed;

/// One second at 192 kHz.
const TAP_FRAMES: usize = 192_000;
/// Display frames a note stays up after the input stops giving one, so a decaying string
/// doesn't flicker.
const HOLD: u32 = 20;
/// Within this many cents a note reads as in tune.
const IN_TUNE: f32 = 3.0;

thread_local! {
    static WINDOWS: std::cell::RefCell<HashMap<usize, adw::Window>> = Default::default();
}

/// The Tuner window's listening. The engine only copies the input into a ring; detection runs
/// here on the UI thread.
pub struct Tuner {
    ring: Option<Consumer<f32>>,
    sample_rate: u32,
    detector: Detector,
    input: Option<(u32, bool)>,
    /// The smoothed pitch being shown, in Hz.
    pub freq: Option<f32>,
    missed: u32,
}

impl Tuner {
    pub fn new(sample_rate: u32) -> Tuner {
        Tuner { ring: None, sample_rate, detector: Detector::new(sample_rate), input: None, freq: None, missed: 0 }
    }

    /// Runs every frame while the window is open: attaches to the engine (again after the audio
    /// server reconnects), follows the track's input and reads the pitch heard since.
    pub fn run(&mut self, handle: &mut Handle, sample_rate: u32, input: Option<(u32, bool)>, mute: bool) {
        if sample_rate != self.sample_rate {
            handle.untap_input();
            *self = Tuner::new(sample_rate);
        }
        let (first, second) = match input {
            Some((i, stereo)) => (Some(i as usize), stereo.then_some(i as usize + 1)),
            None => (None, None),
        };
        if self.ring.as_ref().is_none_or(|r| r.is_abandoned()) {
            self.ring = Some(handle.tap_input(first, second, TAP_FRAMES));
        }
        if input != self.input {
            self.input = input;
            handle.set_tap_input(first, second);
            self.detector.clear();
            self.freq = None;
        }
        handle.shared.mute_main.store(mute, Relaxed);
        if let Some(ring) = self.ring.as_mut()
            && let Ok(chunk) = ring.read_chunk(ring.slots())
        {
            let (a, b) = chunk.as_slices();
            self.detector.push(a);
            self.detector.push(b);
            chunk.commit_all();
        }
        let heard = self.detector.detect();
        self.update(heard);
    }

    fn update(&mut self, heard: Option<f32>) {
        match (heard, self.freq) {
            // Close to what is shown: ease towards it so the needle settles.
            (Some(f), Some(shown)) if (1200.0 * (f / shown).log2()).abs() < 50.0 => {
                self.freq = Some(shown * (f / shown).powf(0.3));
                self.missed = 0;
            }
            (Some(f), _) => {
                self.freq = Some(f);
                self.missed = 0;
            }
            (None, _) => {
                self.missed += 1;
                if self.missed > HOLD {
                    self.freq = None;
                }
            }
        }
    }

    pub fn detach(&mut self, handle: &mut Handle) {
        handle.untap_input();
        handle.shared.mute_main.store(false, Relaxed);
        self.ring = None;
    }
}

fn a4(ctx: &Ctx) -> f32 {
    ctx.s().settings.tuner_a4.clamp(415.0, 466.0)
}

/// Opens the Tuner window, or brings the existing one forward.
pub fn open_window(ctx: &Ctx, parent: &impl IsA<gtk::Window>) {
    let key = ctx.key();
    if let Some(w) = WINDOWS.with(|w| w.borrow().get(&key).cloned()) {
        w.present();
        return;
    }
    ctx.m().tuner_open = true;
    let win = adw::Window::builder()
        .title(format!("{} — Tuner", ctx.s().project.name))
        .default_width(440)
        .default_height(340)
        .transient_for(parent)
        .build();

    let header = adw::HeaderBar::new();
    let reference = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let spin = gtk::SpinButton::with_range(415.0, 466.0, 1.0);
    spin.set_value(a4(ctx) as f64);
    spin.set_tooltip_text(Some("Reference Pitch"));
    {
        let ctx = ctx.clone();
        spin.connect_value_changed(move |b| {
            let mut s = ctx.m();
            s.settings.tuner_a4 = b.value() as f32;
            s.settings.save();
        });
    }
    reference.append(&gtk::Label::new(Some("A4")));
    reference.append(&spin);
    reference.append(&gtk::Label::new(Some("Hz")));
    header.pack_start(&reference);
    let mute = gtk::CheckButton::with_label("Mute Output While Tuning");
    mute.set_tooltip_text(Some("Silence the main outputs while this window is open"));
    mute.set_active(ctx.s().settings.tuner_mute);
    {
        let ctx = ctx.clone();
        mute.connect_toggled(move |b| {
            let mut s = ctx.m();
            s.settings.tuner_mute = b.is_active();
            s.settings.save();
        });
    }
    header.pack_end(&mute);

    let source = gtk::Label::new(None);
    source.add_css_class("dim-label");
    source.set_halign(gtk::Align::Start);
    let phase = Rc::new(Cell::new(0f32));
    let display = {
        let (ctx, phase) = (ctx.clone(), phase.clone());
        Canvas::new(move |c, s, w, h| draw(&ctx, c, s, w, h, phase.get()))
    };
    display.set_hexpand(true);
    display.set_vexpand(true);
    display.set_size_request(360, 220);
    {
        let (ctx, source) = (ctx.clone(), source.clone());
        display.add_tick_callback(move |d, _| {
            let st = ctx.s();
            let text = match (&st.io, st.tuner_track()) {
                (None, _) => "Not connected to an audio server".to_string(),
                (Some(_), Some(t)) => format!("{} · {}", t.name, crate::mixer::input_name(t.input, t.stereo)),
                (Some(_), None) => "Select an audio track to tune its input".to_string(),
            };
            if source.text() != text {
                source.set_text(&text);
            }
            // The strobe drifts right when sharp and left when flat, and stands still in tune.
            if let Some(f) = st.tuner.as_ref().and_then(|t| t.freq) {
                let (_, cents) = pitch::nearest_note(f, st.settings.tuner_a4.clamp(415.0, 466.0));
                phase.set((phase.get() + cents * 0.15).rem_euclid(STRIPE * 2.0));
            }
            drop(st);
            d.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&source);
    content.append(&display);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&content));
    win.set_content(Some(&tv));
    let ctx = ctx.clone();
    win.connect_close_request(move |_| {
        ctx.m().tuner_open = false;
        WINDOWS.with(|w| w.borrow_mut().remove(&key));
        glib::Propagation::Proceed
    });
    WINDOWS.with(|w| w.borrow_mut().insert(key, win.clone()));
    win.present();
}

const STRIPE: f32 = 12.0;

#[allow(clippy::too_many_arguments)]
fn centred(s: &gtk::Snapshot, c: &Canvas, x: f32, y: f32, t: &str, color: &gtk::gdk::RGBA, size: f64, bold: bool) {
    let layout = c.create_pango_layout(Some(t));
    let mut fd = pango::FontDescription::new();
    fd.set_size((size * pango::SCALE as f64) as i32);
    if bold {
        fd.set_weight(pango::Weight::Bold);
    }
    layout.set_font_description(Some(&fd));
    let (w, _) = layout.pixel_size();
    s.save();
    s.translate(&graphene::Point::new(x - w as f32 / 2.0, y));
    s.append_layout(&layout, color);
    s.restore();
}

fn draw(ctx: &Ctx, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32, phase: f32) {
    let fg = canvas::fg(c);
    let dim = |a: f32| gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    rounded(s, 0.0, 0.0, w, h, 8.0, &rgba(0x000000, 0.3));
    let st = ctx.s();
    let reading = st.tuner.as_ref().and_then(|t| t.freq).map(|f| (f, pitch::nearest_note(f, a4(ctx))));
    let mid = w / 2.0;
    let (x0, x1) = (24.0, w - 24.0);
    let x_of = |cents: f32| mid + (cents.clamp(-50.0, 50.0) / 50.0) * (x1 - mid);
    let colour = |cents: f32| match cents.abs() {
        a if a <= IN_TUNE => rgba(0x33d17a, 1.0),
        a if a <= 15.0 => rgba(0xf6d32d, 1.0),
        _ => rgba(0xe01b24, 1.0),
    };

    match reading {
        Some((f, (note, cents))) => {
            centred(s, c, mid, 8.0, &pitch::note_name(note), &colour(cents), 40.0, true);
            centred(s, c, mid, 72.0, &format!("{f:.1} Hz   {cents:+.0} cents"), &dim(0.7), 9.0, false);
        }
        None => centred(s, c, mid, 8.0, "—", &dim(0.35), 40.0, true),
    }

    let scale_y = h * 0.62;
    rect(s, x0, scale_y, x1 - x0, 1.0, &dim(0.3));
    for k in -5i32..=5 {
        let x = x_of(k as f32 * 10.0);
        let major = k == 0 || k.abs() == 5;
        let tick = if major { 12.0 } else { 6.0 };
        rect(s, x - 0.5, scale_y - tick, 1.0, tick, &dim(if major { 0.6 } else { 0.3 }));
        if major {
            let label = match k {
                0 => "0".to_string(),
                k if k < 0 => "−50".to_string(),
                _ => "+50".to_string(),
            };
            centred(s, c, x, scale_y + 4.0, &label, &dim(0.45), 7.0, false);
        }
    }
    rect(s, x_of(-IN_TUNE), scale_y - 3.0, x_of(IN_TUNE) - x_of(-IN_TUNE), 3.0, &rgba(0x33d17a, 0.5));
    if let Some((_, (_, cents))) = reading {
        rounded(s, x_of(cents) - 2.0, scale_y - 44.0, 4.0, 48.0, 2.0, &colour(cents));
    }

    let (band_y, band_h) = (h - 34.0, 18.0);
    rounded(s, x0, band_y, x1 - x0, band_h, 3.0, &dim(0.08));
    if let Some((_, (_, cents))) = reading {
        let col = colour(cents);
        let stripe = gtk::gdk::RGBA::new(col.red(), col.green(), col.blue(), 0.6);
        s.push_clip(&graphene::Rect::new(x0, band_y, x1 - x0, band_h));
        let mut x = x0 - STRIPE * 2.0 + phase;
        while x < x1 {
            rect(s, x, band_y, STRIPE, band_h, &stripe);
            x += STRIPE * 2.0;
        }
        s.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_eases_onto_a_note_and_holds_through_dropouts() {
        let mut t = Tuner::new(48000);
        t.update(Some(110.0));
        assert_eq!(t.freq, Some(110.0));
        t.update(Some(111.0));
        let f = t.freq.unwrap();
        assert!(f > 110.0 && f < 111.0, "{f}");
        t.update(Some(220.0));
        assert_eq!(t.freq, Some(220.0), "a new note jumps straight there");
        for _ in 0..HOLD {
            t.update(None);
        }
        assert_eq!(t.freq, Some(220.0));
        t.update(None);
        assert_eq!(t.freq, None);
    }
}
