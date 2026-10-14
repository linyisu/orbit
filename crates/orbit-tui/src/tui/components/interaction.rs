use ratatui::layout::{Position, Rect};

/// Hover never changes keyboard focus or the selected value. Page transitions
/// retain the physical position so stationary terminal motion cannot refocus.
#[derive(Debug)]
pub(in crate::tui) struct Pointer<T> {
    hovered: Option<T>,
    active: bool,
    position: Option<Position>,
}

impl<T> Default for Pointer<T> {
    fn default() -> Self {
        Self {
            hovered: None,
            active: false,
            position: None,
        }
    }
}

impl<T: Copy> Pointer<T> {
    pub fn hovered(&self) -> Option<T> {
        self.hovered
    }

    pub fn highlight(&self, focus: Option<T>) -> Option<T> {
        if self.active { self.hovered } else { focus }
    }

    pub fn moved(&mut self, point: Position, hits: &[(Rect, T)]) -> bool {
        if self.position == Some(point) {
            return false;
        }
        self.position = Some(point);
        self.active = true;
        self.hovered = hits
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, id)| *id);
        true
    }

    pub fn press(&mut self, point: Position) {
        self.position = Some(point);
        self.active = true;
        self.hovered = None;
    }

    pub fn scroll(&mut self) {
        self.active = true;
        self.hovered = None;
    }

    pub fn invalidate(&mut self) {
        self.hovered = None;
    }

    pub fn keyboard(&mut self) {
        *self = Self::default();
    }
}
