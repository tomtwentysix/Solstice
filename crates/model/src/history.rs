use crate::Project;



/// Snapshot undo. Audio lives on disk, so a project clone is small and every edit is
/// trivially reversible without per-command inverse logic.
pub struct History {
    undo: Vec<(String, Project)>,
    redo: Vec<(String, Project)>,
    dirty: bool,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        History { undo: Vec::new(), redo: Vec::new(), dirty: false, limit: 200 }
    }
}

impl History {
    /// Call before mutating `current`.
    pub fn push(&mut self, label: &str, current: &Project) {
        self.push_snapshot(label, current.clone());
    }

    /// Folds the latest undo step into the one before, keeping the latest label, for edits
    /// that are made in two steps but undo as one.
    pub fn merge_last(&mut self) {
        if self.undo.len() >= 2 {
            let (label, _) = self.undo.pop().unwrap();
            if let Some(last) = self.undo.last_mut() {
                last.0 = label;
            }
        }
    }

    /// Records an edit that has already been applied, given the project as it was before.
    pub fn push_snapshot(&mut self, label: &str, before: Project) {
        self.undo.push((label.to_string(), before));
        while self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    pub fn undo(&mut self, current: &mut Project) -> Option<String> {
        let (label, prev) = self.undo.pop()?;
        self.redo.push((label.clone(), std::mem::replace(current, prev)));
        self.dirty = true;
        Some(label)
    }

    /// Takes back the latest edit as if it had never been made, leaving nothing to redo.
    pub fn retract(&mut self, current: &mut Project) -> Option<String> {
        let (label, prev) = self.undo.pop()?;
        *current = prev;
        Some(label)
    }

    pub fn redo(&mut self, current: &mut Project) -> Option<String> {
        let (label, next) = self.redo.pop()?;
        self.undo.push((label.clone(), std::mem::replace(current, next)));
        self.dirty = true;
        Some(label)
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|(l, _)| l.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|(l, _)| l.as_str())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        let limit = self.limit;
        *self = History { limit, ..History::default() };
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        while self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrackKind;

    #[test]
    fn undo_redo() {
        let mut p = Project::new("T", 48000);
        let mut h = History::default();
        h.push("Add Track", &p);
        p.add_track(TrackKind::Audio);
        assert_eq!(h.undo(&mut p).as_deref(), Some("Add Track"));
        assert!(p.tracks.is_empty());
        h.redo(&mut p);
        assert_eq!(p.tracks.len(), 1);
        assert!(h.redo(&mut p).is_none());
    }

    #[test]
    fn retract_leaves_nothing_to_redo() {
        let mut p = Project::new("T", 48000);
        let mut h = History::default();
        h.push("Add Track", &p);
        p.add_track(TrackKind::Audio);
        h.push("Add Bus", &p);
        p.add_bus();
        assert_eq!(h.retract(&mut p).as_deref(), Some("Add Bus"));
        assert!(p.buses.is_empty() && p.tracks.len() == 1);
        assert!(h.redo_label().is_none());
        assert_eq!(h.undo_label(), Some("Add Track"));
    }
}
