use crate::canvas::{self, Canvas, rgba, rounded, text};
use crate::studio::Ctx;
use gtk::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

const XRUN_WINDOW: Duration = Duration::from_secs(3600);
const XRUN_THRESHOLD: usize = 10;

/// How many xruns each update saw and when, from the engine's running total, for counting only recent ones.
#[derive(Default)]
struct RecentXruns {
    seen: u32,
    recent: usize,
    times: VecDeque<(Instant, usize)>,
}

impl RecentXruns {
    /// Returns how many xruns happened in the last hour. A total below the last one seen
    /// means the engine was replaced, which starts its count from zero again.
    fn update(&mut self, total: u32, now: Instant) -> usize {
        if total < self.seen {
            self.seen = 0;
        }
        if total > self.seen {
            let n = (total - self.seen) as usize;
            self.times.push_back((now, n));
            self.recent += n;
        }
        self.seen = total;
        while let Some(&(t, n)) = self.times.front()
            && now.duration_since(t) > XRUN_WINDOW
        {
            self.times.pop_front();
            self.recent -= n;
        }
        self.recent
    }
}

/// The centre of the control bar: transport buttons and the LCD.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    transport.add_css_class("linked");
    let button = |icon: &str, tip: &str, action: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.set_tooltip_text(Some(tip));
        b.set_action_name(Some(action));
        transport.append(&b);
        b
    };
    button("media-seek-backward-symbolic", "Rewind (,)", "win.rewind");
    button("media-seek-forward-symbolic", "Forward (.)", "win.forward");
    button("media-playback-stop-symbolic", "Stop", "win.stop");
    let play = button("media-playback-start-symbolic", "Play (Space)", "win.play");
    let rec = button("media-record-symbolic", "Record (R)", "win.record");
    rec.add_css_class("record-button");
    row.append(&transport);

    let lcd = {
        let ctx = ctx.clone();
        let xruns = RefCell::new(RecentXruns::default());
        Canvas::new(move |c, s, w, h| draw_lcd(&ctx, &xruns, c, s, w, h))
    };
    lcd.set_size_request(330, 38);
    lcd.add_css_class("lcd");
    {
        let (ctx, play, rec) = (ctx.clone(), play.clone(), rec.clone());
        let last = std::cell::Cell::new((false, false));
        lcd.add_tick_callback(move |l, _| {
            l.queue_draw();
            let state = (ctx.playing(), ctx.recording());
            if state != last.get() {
                last.set(state);
                play.set_icon_name(if state.0 { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
                if state.1 {
                    rec.add_css_class("recording");
                } else {
                    rec.remove_css_class("recording");
                }
            }
            glib::ControlFlow::Continue
        });
    }
    let tempo_click = gtk::GestureClick::new();
    {
        let (ctx, lcd2) = (ctx.clone(), lcd.clone());
        tempo_click.connect_released(move |_, _, x, _| {
            if x > 200.0 {
                tempo_popover(&ctx, &lcd2, x);
            }
        });
    }
    lcd.add_controller(tempo_click);
    row.append(&lcd);

    let modes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    modes.add_css_class("linked");
    let cycle = gtk::ToggleButton::new();
    cycle.set_icon_name("media-playlist-repeat-symbolic");
    cycle.set_tooltip_text(Some("Cycle (C)"));
    cycle.set_action_name(Some("win.cycle"));
    let punch = gtk::ToggleButton::with_label("Punch");
    punch.set_tooltip_text(Some("Autopunch"));
    punch.set_action_name(Some("win.punch"));
    let metro = gtk::ToggleButton::new();
    metro.set_icon_name("alarm-symbolic");
    metro.set_tooltip_text(Some("Metronome (K)"));
    metro.set_action_name(Some("win.metronome"));
    modes.append(&cycle);
    modes.append(&punch);
    modes.append(&metro);
    row.append(&modes);

    let reference = gtk::ToggleButton::with_label("Reference");
    reference.set_tooltip_text(Some("Play the Reference Tracks Instead of the Mix (O)"));
    reference.set_action_name(Some("win.reference"));
    row.append(&reference);

    let tuner = gtk::Button::from_icon_name("solstice-tuner-symbolic");
    tuner.set_tooltip_text(Some("Tuner (Shift+T)"));
    tuner.set_action_name(Some("win.show-tuner"));
    row.append(&tuner);

    row.upcast()
}

fn draw_lcd(ctx: &Ctx, xruns: &RefCell<RecentXruns>, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let fg = canvas::fg(c);
    rounded(s, 0.0, 0.0, w, h, 6.0, &gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.08));
    let st = ctx.s();
    let p = &st.project;
    let time = p.time();
    let pos = st.io.as_ref().map_or(0, |io| io.handle.shared.position.load(Relaxed));
    let (bar, beat, div, tick) = time.position(pos);
    text(s, c, 10.0, 2.0, &format!("{bar:>3} {beat} {div} {tick:>3}"), &fg, Some(15.0));
    let secs = time.seconds(pos);
    let dim = gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.6);
    text(
        s,
        c,
        10.0,
        h - 15.0,
        &format!("{:02}:{:02}:{:02}.{:03}", (secs / 3600.0) as u32, (secs / 60.0) as u32 % 60, secs as u32 % 60, (secs.fract() * 1000.0) as u32),
        &dim,
        Some(7.5),
    );
    text(s, c, 150.0, 2.0, "BAR  BEAT", &dim, Some(6.5));
    text(s, c, 210.0, 2.0, &format!("{:.1}", time.tempo_at(pos)), &fg, Some(15.0));
    text(s, c, 210.0, h - 15.0, "TEMPO", &dim, Some(6.5));
    let (num, den) = time.sig_at(pos);
    text(s, c, 280.0, 2.0, &format!("{num}/{den}"), &fg, Some(15.0));
    if let Some(io) = &st.io {
        let load = io.handle.shared.load.get().clamp(0.0, 1.0);
        let col = if load > 0.8 { rgba(0xe01b24, 1.0) } else { canvas::accent(c) };
        rounded(s, 150.0, h - 12.0, 50.0, 4.0, 2.0, &dim);
        rounded(s, 150.0, h - 12.0, 50.0 * load, 4.0, 2.0, &col);
        let xr = xruns.borrow_mut().update(io.handle.shared.xruns.load(Relaxed), Instant::now());
        let pdc = io.handle.shared.pdc.load(Relaxed);
        if xr > XRUN_THRESHOLD {
            text(s, c, 280.0, h - 15.0, &format!("{xr} xrun"), &rgba(0xe01b24, 1.0), Some(6.5));
        } else if pdc > 0 {
            let ms = pdc as f64 * 1000.0 / p.sample_rate as f64;
            text(s, c, 280.0, h - 15.0, &format!("PDC {ms:.1} ms"), &dim, Some(6.5));
        }
    }
}

fn tempo_popover(ctx: &Ctx, lcd: &Canvas, x: f64) {
    let pop = gtk::Popover::new();
    pop.set_parent(lcd);
    pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, 0, 1, 38)));
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(6).build();
    let (tempo, num, den) = {
        let p = &ctx.s().project;
        (p.tempo, p.sig_num, p.sig_den)
    };
    let t = gtk::SpinButton::with_range(20.0, 400.0, 0.5);
    t.set_digits(1);
    t.set_value(tempo);
    let n = gtk::SpinButton::with_range(1.0, 32.0, 1.0);
    n.set_value(num as f64);
    let d = gtk::DropDown::from_strings(&["2", "4", "8", "16"]);
    d.set_selected(match den {
        2 => 0,
        8 => 2,
        16 => 3,
        _ => 1,
    });
    grid.attach(&gtk::Label::new(Some("Tempo")), 0, 0, 1, 1);
    grid.attach(&t, 1, 0, 2, 1);
    grid.attach(&gtk::Label::new(Some("Signature")), 0, 1, 1, 1);
    grid.attach(&n, 1, 1, 1, 1);
    grid.attach(&d, 2, 1, 1, 1);
    pop.set_child(Some(&grid));
    let shown = (t.value(), n.value() as u8, d.selected());
    let ctx = ctx.clone();
    pop.connect_closed(move |p| {
        let (old_tempo, old_num, old_den) = {
            let p = &ctx.s().project;
            (p.tempo, p.sig_num, p.sig_den)
        };
        let tempo = edited(old_tempo, shown.0, t.value());
        let num = edited(old_num, shown.1, n.value() as u8);
        let den = edited(old_den, [2u8, 4, 8, 16][shown.2 as usize], [2u8, 4, 8, 16][d.selected() as usize]);
        let changed = (old_tempo - tempo).abs() > 1e-6 || old_num != num || old_den != den;
        if changed {
            ctx.edit("Change Tempo", |p| {
                let old = p.time();
                p.tempo = tempo;
                p.sig_num = num;
                p.sig_den = den;
                p.retime(&old);
            });
        }
        p.unparent();
    });
    pop.popup();
}

/// A project value the controls may not be able to show (an imported 32nd-note signature, say)
/// survives unless its control moved from where it opened.
pub fn edited<T: PartialEq>(project: T, shown: T, now: T) -> T {
    if now == shown { project } else { now }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_the_last_hour() {
        let start = Instant::now();
        let mut x = RecentXruns::default();
        assert_eq!(x.update(4, start), 4);
        assert_eq!(x.update(11, start + Duration::from_secs(1800)), 11);
        assert_eq!(x.update(11, start + Duration::from_secs(3601)), 7);
        assert_eq!(x.update(12, start + Duration::from_secs(5401)), 1);
    }

    #[test]
    fn untouched_controls_keep_the_project_value() {
        assert_eq!(edited(15.0, 20.0, 20.0), 15.0);
        assert_eq!(edited(32u8, 4, 4), 32);
        assert_eq!(edited(15.0, 20.0, 96.5), 96.5);
        assert_eq!(edited(32u8, 4, 8), 8);
    }

    #[test]
    fn a_new_engine_starts_again() {
        let start = Instant::now();
        let mut x = RecentXruns::default();
        assert_eq!(x.update(20, start), 20);
        assert_eq!(x.update(3, start + Duration::from_secs(1)), 23);
    }
}
