use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, gsk};
use std::cell::RefCell;

type DrawFn = Box<dyn Fn(&Canvas, &gtk::Snapshot, f32, f32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub draw: RefCell<Option<DrawFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "SolsticeCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {}

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if let Some(draw) = self.draw.borrow().as_ref() {
                draw(&obj, snapshot, obj.width() as f32, obj.height() as f32);
            }
        }
    }
}

glib::wrapper! {
    /// A widget drawn entirely with GSK render nodes, so it renders on the GPU.
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Canvas {
    pub fn new(draw: impl Fn(&Canvas, &gtk::Snapshot, f32, f32) + 'static) -> Self {
        let c: Canvas = glib::Object::new();
        c.imp().draw.replace(Some(Box::new(draw)));
        c
    }
}

pub fn rgba(hex: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
        alpha,
    )
}

pub fn rect(s: &gtk::Snapshot, x: f32, y: f32, w: f32, h: f32, c: &gdk::RGBA) {
    if w > 0.0 && h > 0.0 {
        s.append_color(c, &graphene::Rect::new(x, y, w, h));
    }
}

pub fn rounded(s: &gtk::Snapshot, x: f32, y: f32, w: f32, h: f32, r: f32, c: &gdk::RGBA) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let rr = gsk::RoundedRect::from_rect(graphene::Rect::new(x, y, w, h), r.min(w / 2.0).min(h / 2.0));
    s.push_rounded_clip(&rr);
    s.append_color(c, &graphene::Rect::new(x, y, w, h));
    s.pop();
}

pub fn text(s: &gtk::Snapshot, widget: &impl IsA<gtk::Widget>, x: f32, y: f32, t: &str, c: &gdk::RGBA, size: Option<f64>) {
    let layout = widget.create_pango_layout(Some(t));
    if let Some(pt) = size {
        let mut fd = gtk::pango::FontDescription::new();
        fd.set_size((pt * gtk::pango::SCALE as f64) as i32);
        layout.set_font_description(Some(&fd));
    }
    s.save();
    s.translate(&graphene::Point::new(x, y));
    s.append_layout(&layout, c);
    s.restore();
}

pub fn accent(widget: &impl IsA<gtk::Widget>) -> gdk::RGBA {
    #[allow(deprecated)]
    widget.style_context().lookup_color("accent_bg_color").unwrap_or_else(|| rgba(0x3584e4, 1.0))
}

pub fn fg(widget: &impl IsA<gtk::Widget>) -> gdk::RGBA {
    widget.color()
}

/// Logic-like track colours: blue, green, yellow, orange, red, purple, teal, pink.
pub fn track_color(i: u8) -> gdk::RGBA {
    const C: [u32; 8] = [0x3584e4, 0x33d17a, 0xf6d32d, 0xff7800, 0xe01b24, 0x9141ac, 0x2ec27e, 0xdc8add];
    rgba(C[i as usize % C.len()], 1.0)
}

/// Fills a waveform outline built from min/max pairs, one pair per pixel column.
pub fn waveform(s: &gtk::Snapshot, x: f32, y: f32, h: f32, cols: &[(f32, f32)], c: &gdk::RGBA) {
    if cols.is_empty() {
        return;
    }
    let mid = y + h / 2.0;
    let half = h / 2.0;
    let b = gsk::PathBuilder::new();
    b.move_to(x, mid - cols[0].1.clamp(-1.0, 1.0) * half);
    for (i, (_, hi)) in cols.iter().enumerate() {
        b.line_to(x + i as f32 + 0.5, mid - hi.clamp(-1.0, 1.0) * half);
    }
    for (i, (lo, _)) in cols.iter().enumerate().rev() {
        b.line_to(x + i as f32 + 0.5, mid - lo.clamp(-1.0, 1.0) * half);
    }
    b.close();
    s.append_fill(&b.to_path(), gsk::FillRule::Winding, c);
}
