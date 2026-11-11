use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamDef {
    pub id: u32,
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: &'static str,
    /// 0 for continuous; otherwise the number of steps above the minimum.
    pub steps: u32,
    /// Map the 0..1 control logarithmically, for times and frequencies.
    pub log: bool,
}

impl ParamDef {
    pub const fn new(id: u32, name: &'static str, min: f32, max: f32, default: f32, unit: &'static str) -> ParamDef {
        ParamDef { id, name, min, max, default, unit, steps: 0, log: false }
    }

    pub const fn log(mut self) -> ParamDef {
        self.log = true;
        self
    }

    pub const fn steps(mut self, n: u32) -> ParamDef {
        self.steps = n;
        self
    }

    pub fn to_norm(&self, v: f32) -> f32 {
        let v = v.clamp(self.min, self.max);
        if self.log && self.min > 0.0 {
            (v / self.min).ln() / (self.max / self.min).ln()
        } else {
            (v - self.min) / (self.max - self.min)
        }
    }

    pub fn from_norm(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        let v = if self.log && self.min > 0.0 {
            self.min * (self.max / self.min).powf(n)
        } else {
            self.min + n * (self.max - self.min)
        };
        if self.steps > 0 {
            let step = (self.max - self.min) / self.steps as f32;
            self.min + ((v - self.min) / step).round() * step
        } else {
            v
        }
    }
}

/// A native plug-in's parameters, shared between its controls (UI thread) and its DSP (RT
/// thread) as plain values in atomics.
pub struct Bank {
    pub defs: &'static [ParamDef],
    values: Vec<AtomicU32>,
    /// Gain reduction in dB, or another level the plug-in reports to its UI.
    pub meter: AtomicU32,
}

impl Bank {
    pub fn new(defs: &'static [ParamDef]) -> Bank {
        Bank {
            defs,
            values: defs.iter().map(|d| AtomicU32::new(d.default.to_bits())).collect(),
            meter: AtomicU32::new(0f32.to_bits()),
        }
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.defs.iter().position(|d| d.id == id)
    }

    pub fn get(&self, id: u32) -> f32 {
        self.index(id).map_or(0.0, |i| f32::from_bits(self.values[i].load(Relaxed)))
    }

    pub fn set(&self, id: u32, v: f32) {
        if let Some(i) = self.index(id) {
            let d = &self.defs[i];
            self.values[i].store(v.clamp(d.min, d.max).to_bits(), Relaxed);
        }
    }

    pub fn norm(&self, id: u32) -> f32 {
        self.index(id).map_or(0.0, |i| self.defs[i].to_norm(self.get(id)))
    }

    pub fn set_norm(&self, id: u32, n: f32) {
        if let Some(i) = self.index(id) {
            self.set(id, self.defs[i].from_norm(n));
        }
    }

    pub fn text(&self, id: u32, v: f32) -> String {
        let Some(i) = self.index(id) else { return String::new() };
        let d = &self.defs[i];
        match d.unit {
            "" if d.steps == 1 => (if v > 0.5 { "On" } else { "Off" }).into(),
            "" => format!("{v:.2}"),
            "Hz" if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
            "%" => format!("{v:.0} %"),
            "ms" if v >= 1000.0 => format!("{:.2} s", v / 1000.0),
            u if v.abs() < 10.0 => format!("{v:.1} {u}"),
            u => format!("{v:.0} {u}"),
        }
    }

    pub fn meter(&self) -> f32 {
        f32::from_bits(self.meter.load(Relaxed))
    }

    pub fn set_meter(&self, v: f32) {
        self.meter.store(v.to_bits(), Relaxed);
    }

    /// Saved state: one `id=value` line per parameter.
    pub fn state(&self) -> Vec<u8> {
        self.defs.iter().map(|d| format!("{}={}\n", d.id, self.get(d.id))).collect::<String>().into_bytes()
    }

    pub fn set_state(&self, data: &[u8]) -> bool {
        let Ok(text) = std::str::from_utf8(data) else { return false };
        for line in text.lines() {
            if let Some((id, v)) = line.split_once('=')
                && let (Ok(id), Ok(v)) = (id.trim().parse(), v.trim().parse())
            {
                self.set(id, v);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static DEFS: [ParamDef; 2] = [ParamDef::new(0, "Gain", -24.0, 24.0, 0.0, "dB"), ParamDef::new(1, "Freq", 20.0, 20000.0, 1000.0, "Hz").log()];

    #[test]
    fn values_and_state() {
        let b = Bank::new(&DEFS);
        assert_eq!(b.get(1), 1000.0);
        b.set_norm(0, 0.75);
        assert_eq!(b.get(0), 12.0);
        assert!((b.defs[1].from_norm(b.norm(1)) - 1000.0).abs() < 0.01);
        b.set(0, 99.0);
        assert_eq!(b.get(0), 24.0);
        let s = b.state();
        let c = Bank::new(&DEFS);
        assert!(c.set_state(&s));
        assert_eq!(c.get(0), 24.0);
    }
}
