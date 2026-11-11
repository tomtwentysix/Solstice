#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorSource {
    pub input: bool,
    pub playback: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct MonitorState {
    pub armed: bool,
    /// The track's input monitoring button.
    pub monitor: bool,
    pub playing: bool,
    pub recording: bool,
    /// False while punch-in is on and the playhead is outside the punch range.
    pub in_punch: bool,
    pub auto_input: bool,
}

/// Logic's auto input monitoring: an armed track hears its input when stopped and while it is
/// actually recording, and its regions otherwise, so you can rehearse over the old take up to
/// the punch-in point. The input monitoring button always adds the input.
pub fn monitor_source(s: MonitorState) -> MonitorSource {
    let rolling = s.playing || s.recording;
    if !s.armed {
        return MonitorSource { input: s.monitor, playback: rolling };
    }
    let capturing = s.recording && s.in_punch;
    let input = if s.auto_input { !rolling || capturing } else { true };
    MonitorSource { input: input || s.monitor, playback: rolling && !capturing }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(armed: bool, playing: bool, recording: bool, in_punch: bool) -> MonitorState {
        MonitorState { armed, monitor: false, playing, recording, in_punch, auto_input: true }
    }

    #[test]
    fn auto_input() {
        assert_eq!(monitor_source(st(true, false, false, true)), MonitorSource { input: true, playback: false });
        assert_eq!(monitor_source(st(true, true, false, true)), MonitorSource { input: false, playback: true });
        assert_eq!(monitor_source(st(true, true, true, true)), MonitorSource { input: true, playback: false });
        assert_eq!(monitor_source(st(true, true, true, false)), MonitorSource { input: false, playback: true });
        assert_eq!(monitor_source(st(false, true, true, true)), MonitorSource { input: false, playback: true });
    }
}
