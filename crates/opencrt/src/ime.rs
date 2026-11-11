//! Terminal IME state: when the terminal owns the input method, where its
//! candidate window belongs, and the composition text shown while typing.

/// A rectangle in logical window or terminal-area coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Keep tick-to-tick layout jitter from repeatedly updating winit.
    fn quantize(self) -> Self {
        let half = |v: f32| (v * 2.0).round() / 2.0;
        Self { x: half(self.x), y: half(self.y), width: half(self.width), height: half(self.height) }
    }

    pub fn in_window(self, origin: (f32, f32)) -> Self {
        Self { x: self.x + origin.0, y: self.y + origin.1, ..self }.quantize()
    }
}

/// Whether the terminal should manage winit's IME state on this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// A Slint TextInput owns IME: leave its Enable/Disable requests alone.
    Defer,
    Allow(bool),
}

/// Slint's TextInputs manage their own IME. Everything else belongs to the
/// terminal only while it has focus and can accept typed text.
pub fn decision(
    terminal_focused: bool,
    dialog_open: bool,
    text_input_focused: bool,
    remote_desktop: bool,
    window_active: bool,
) -> Decision {
    if text_input_focused {
        Decision::Defer
    } else {
        Decision::Allow(terminal_focused && !dialog_open && !remote_desktop && window_active)
    }
}

/// Approximate terminal columns occupied by composition text. Pinyin and other
/// ASCII are narrow; CJK ideographs, full-width punctuation and emoji are wide.
pub fn text_columns(text: &str) -> f32 {
    text.chars().map(char_columns).sum::<f32>().max(1.0)
}

fn char_columns(c: char) -> f32 {
    let n = c as u32;
    let wide = matches!(
        n,
        0x1100..=0x115F
            | 0x2329..=0x232A
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    );
    if wide { 2.0 } else { 1.0 }
}

/// The composition/cursor cell's rectangle, relative to the terminal area.
/// `cursor` and `size` are 1-based row/column and column/row counts respectively.
pub fn composition_rect(
    pane: [f32; 4],
    cell: (f32, f32),
    zoom: (f32, f32),
    size: (usize, usize),
    cursor: (usize, usize),
    columns: f32,
) -> Rect {
    let (cell_w, cell_h) = ((cell.0 * zoom.0).max(1.0), (cell.1 * zoom.1).max(1.0));
    let (pane_x, pane_y, pane_w, pane_h) = (pane[0], pane[1], pane[2].max(0.0), pane[3].max(0.0));
    let (right, bottom) = (pane_x + pane_w, pane_y + pane_h);
    let col = cursor.1.saturating_sub(1).min(size.0.saturating_sub(1)) as f32;
    let row = cursor.0.saturating_sub(1).min(size.1.saturating_sub(1)) as f32;
    let x = (pane_x + col * cell_w).clamp(pane_x, (right - cell_w).max(pane_x));
    let y = (pane_y + row * cell_h).clamp(pane_y, (bottom - cell_h).max(pane_y));
    Rect {
        x,
        y,
        width: (columns.max(1.0) * cell_w).min(right - x).max(0.0),
        height: cell_h.min(bottom - y).max(0.0),
    }
    .quantize()
}

#[derive(Debug)]
pub struct State {
    preedit: String,
    window_active: bool,
    allowed: Option<bool>,
    cursor_area: Option<Rect>,
    overlay: Option<(String, Rect)>,
}

impl Default for State {
    fn default() -> Self {
        Self { preedit: String::new(), window_active: true, allowed: None, cursor_area: None, overlay: None }
    }
}

impl State {
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    pub fn window_active(&self) -> bool {
        self.window_active
    }

    pub fn set_preedit(&mut self, text: String) {
        self.preedit = text;
    }

    /// Forget uncommitted text on disable, focus loss, tab or dialog changes.
    pub fn clear_preedit(&mut self) {
        self.preedit.clear();
    }

    pub fn set_window_focused(&mut self, active: bool) {
        self.window_active = active;
        if !active {
            self.clear_preedit();
        }
    }

    /// Returns the new winit IME permission only when it changed. A deferred
    /// tick invalidates the cache so Slint can manage its TextInput undisturbed.
    pub fn sync_allowed(&mut self, decision: Decision) -> Option<bool> {
        match decision {
            Decision::Defer => {
                self.allowed = None;
                self.cursor_area = None;
                self.clear_preedit();
                None
            }
            Decision::Allow(want) => {
                if !want {
                    self.clear_preedit();
                    self.cursor_area = None;
                }
                if self.allowed == Some(want) {
                    None
                } else {
                    self.allowed = Some(want);
                    Some(want)
                }
            }
        }
    }

    /// True when winit's candidate rectangle should be updated.
    pub fn sync_cursor_area(&mut self, rect: Rect) -> bool {
        let rect = rect.quantize();
        if self.cursor_area == Some(rect) {
            false
        } else {
            self.cursor_area = Some(rect);
            true
        }
    }

    /// Returns the new pre-edit text and rectangle only when either changed.
    pub fn sync_overlay(&mut self, rect: Rect) -> Option<(String, Rect)> {
        let rect = rect.quantize();
        let unchanged = self.overlay.as_ref().is_some_and(|(text, shown)| text == &self.preedit && *shown == rect);
        if unchanged {
            None
        } else {
            let shown = (self.preedit.clone(), rect);
            self.overlay = Some(shown.clone());
            Some(shown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_focused_terminal_owns_ime() {
        assert_eq!(decision(true, false, false, false, true), Decision::Allow(true));
        assert_eq!(decision(false, false, false, false, true), Decision::Allow(false));
        assert_eq!(decision(true, true, false, false, true), Decision::Allow(false));
        assert_eq!(decision(true, false, false, true, true), Decision::Allow(false));
        assert_eq!(decision(true, false, false, false, false), Decision::Allow(false));
        assert_eq!(decision(false, false, true, false, true), Decision::Defer);
    }

    #[test]
    fn text_input_focus_defers_and_clears_composition() {
        let mut state = State::default();
        assert_eq!(state.sync_allowed(Decision::Allow(true)), Some(true));
        assert_eq!(state.sync_allowed(Decision::Allow(true)), None);
        state.set_preedit("ni".into());
        assert_eq!(state.sync_allowed(Decision::Defer), None);
        assert_eq!(state.preedit(), "");
        // Slint may have disabled IME in the meantime; the terminal re-enables it.
        assert_eq!(state.sync_allowed(Decision::Allow(true)), Some(true));
        state.set_preedit("中".into());
        assert_eq!(state.sync_allowed(Decision::Allow(false)), Some(false));
        assert_eq!(state.preedit(), "");
    }

    #[test]
    fn window_focus_loss_clears_composition() {
        let mut state = State::default();
        assert!(state.window_active());
        state.set_preedit("hao".into());
        state.set_window_focused(false);
        assert!(!state.window_active());
        assert_eq!(state.preedit(), "");
        state.set_window_focused(true);
        assert!(state.window_active());
    }

    #[test]
    fn composition_follows_one_based_cursor_and_zoom() {
        let pane = [10.0, 20.0, 100.0, 60.0];
        let rect = composition_rect(pane, (8.0, 16.0), (1.5, 1.5), (10, 3), (2, 3), text_columns("中"));
        assert_eq!(rect, Rect { x: 34.0, y: 44.0, width: 24.0, height: 24.0 });
        assert_eq!(rect.in_window((4.0, 2.0)), Rect { x: 38.0, y: 46.0, width: 24.0, height: 24.0 });
        assert_eq!(text_columns(""), 1.0);
        assert_eq!(text_columns("ni"), 2.0);
        assert_eq!(text_columns("中文"), 4.0);
    }

    #[test]
    fn composition_is_clamped_to_the_pane() {
        let rect = composition_rect([10.0, 20.0, 20.0, 20.0], (8.0, 16.0), (1.0, 1.0), (10, 3), (99, 99), 4.0);
        assert_eq!(rect, Rect { x: 22.0, y: 24.0, width: 8.0, height: 16.0 });
    }

    #[test]
    fn geometry_updates_are_deduplicated() {
        let mut state = State::default();
        let a = Rect { x: 10.0, y: 20.0, width: 8.0, height: 16.0 };
        let b = Rect { x: 10.2, y: 20.0, width: 8.0, height: 16.0 };
        assert!(state.sync_cursor_area(a));
        assert!(!state.sync_cursor_area(a));
        assert!(!state.sync_cursor_area(b));
        assert!(state.sync_cursor_area(Rect { x: 12.0, ..a }));
        assert_eq!(state.sync_overlay(a), Some((String::new(), a)));
        assert_eq!(state.sync_overlay(a), None);
        state.set_preedit("中".into());
        assert_eq!(state.sync_overlay(a), Some(("中".to_string(), a)));
    }
}
