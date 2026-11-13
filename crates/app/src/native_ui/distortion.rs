use super::*;
use solstice_dsp::effects::Saturation;

pub fn saturation(b: &Arc<Bank>) -> gtk::Widget {
    // Input sample value across, output down, with the input peak as a dot on the curve.
    let graph = {
        let b = b.clone();
        picture(240, move |c, s, w, h| {
            let fg = canvas::fg(c);
            let (mode, drive, out, mix) = (b.get(0).round() as usize, b.get(1), b.get(3), b.get(4) / 100.0);
            let curve = |x: f32| Saturation::transfer(mode, drive, out, mix, x);
            let range = 1.25;
            let (cx, cy) = (w / 2.0, h / 2.0);
            let half = (w.min(h) / 2.0 - 16.0).max(10.0);
            let x = |v: f32| cx + half * v / range;
            let y = |v: f32| cy - half * v.clamp(-range, range) / range;
            for v in [-1.0, -0.5, 0.5, 1.0] {
                canvas::rect(s, x(v), y(range), 1.0, 2.0 * half, &alpha(&fg, 0.06));
                canvas::rect(s, x(-range), y(v), 2.0 * half, 1.0, &alpha(&fg, 0.06));
            }
            canvas::rect(s, x(0.0), y(range), 1.0, 2.0 * half, &alpha(&fg, 0.15));
            canvas::rect(s, x(-range), y(0.0), 2.0 * half, 1.0, &alpha(&fg, 0.15));
            stroke(s, &[(x(-1.0), y(-1.0)), (x(1.0), y(1.0))], 1.0, &alpha(&fg, 0.25));
            let pts: Vec<(f32, f32)> = (0..=200).map(|i| -1.0 + i as f32 * 0.01).map(|v| (x(v), y(curve(v)))).collect();
            stroke(s, &pts, 2.5, &canvas::accent(c));
            let peak = b.meter().min(1.0);
            if peak > 0.001 {
                let dot = gsk::PathBuilder::new();
                dot.add_circle(&graphene::Point::new(x(peak), y(curve(peak))), 5.0);
                s.append_fill(&dot.to_path(), gsk::FillRule::Winding, &canvas::rgba(0xe5a50a, 0.9));
            }
            label(s, c, x(-range) + 4.0, y(range) + 2.0, "Output", 0.5);
            label_right(s, c, x(range) - 4.0, y(0.0) + 4.0, "Input", 0.5);
            label_right(s, c, w - 12.0, 10.0, &b.text(0, b.get(0)), 0.8);
        })
    };
    stack(&[
        graph.upcast_ref(),
        columns(&[section("Drive", None, b, &[0, 1]), section("Tone", None, b, &[2]), section("Output", None, b, &[3, 4])]).upcast_ref(),
    ])
    .upcast()
}
