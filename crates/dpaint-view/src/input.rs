//! Window events reduced to intent.
//!
//! No `winit` types appear here on purpose: the binary translates its events into these
//! small enums and everything downstream — including the tests — deals in intent rather
//! than in platform specifics.

/// Which kind of document the window is showing. Model documents orbit; raster and
/// vector documents pan, because there is nothing to orbit around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Model,
    Canvas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

impl Mods {
    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Default::default()
        }
    }
}

/// What a held button does while the mouse moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    Orbit,
    Pan,
    None,
}

/// A keypress that changes the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `f` — frame the subject.
    Frame,
    /// `1` — reset to the default view.
    Reset,
    /// `c` — transparency checkerboard on 2D documents.
    ToggleChecker,
    /// `p` — nearest-neighbour magnification on 2D documents.
    TogglePixelated,
    /// `s` — hide or show the status readout.
    ToggleStatus,
    /// `r` — force a rebuild from disk, without waiting for the journal poll.
    Reload,
    /// `q` or Escape.
    Quit,
}

/// The gesture a button starts. Left-drag orbits a model; shift makes it pan, and the
/// right and middle buttons always pan, so a trackpad with no middle button and a mouse
/// with no modifier both work.
pub fn drag_for(mode: Mode, button: Button, mods: Mods) -> Drag {
    match (mode, button) {
        (_, Button::Right) | (_, Button::Middle) => Drag::Pan,
        (Mode::Canvas, Button::Left) => Drag::Pan,
        (Mode::Model, Button::Left) => {
            if mods.shift {
                Drag::Pan
            } else {
                Drag::Orbit
            }
        }
    }
}

/// Map a key — already lowercased by the caller, as the platform reports it — to an
/// action. `escape` and `q` both quit.
pub fn action_for(key: &str) -> Option<Action> {
    match key {
        "f" => Some(Action::Frame),
        "1" => Some(Action::Reset),
        "c" => Some(Action::ToggleChecker),
        "p" => Some(Action::TogglePixelated),
        "s" => Some(Action::ToggleStatus),
        "r" => Some(Action::Reload),
        "q" | "escape" => Some(Action::Quit),
        _ => None,
    }
}

/// One wheel notch, from either a line-based wheel or a pixel-precise trackpad. Trackpad
/// deltas arrive in pixels and are far larger per event, so they are scaled down to the
/// same "notches" a mouse wheel produces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wheel {
    Lines(f32),
    Pixels(f32),
}

/// Pixels of trackpad scroll that equal one mouse-wheel notch.
pub const PIXELS_PER_NOTCH: f32 = 40.0;

/// Clamp per event so one flick of a high-resolution trackpad cannot cross the whole
/// zoom range in a single frame.
pub const MAX_NOTCHES_PER_EVENT: f32 = 4.0;

pub fn notches(w: Wheel) -> f32 {
    let raw = match w {
        Wheel::Lines(y) => y,
        Wheel::Pixels(y) => y / PIXELS_PER_NOTCH,
    };
    raw.clamp(-MAX_NOTCHES_PER_EVENT, MAX_NOTCHES_PER_EVENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_drag_orbits_a_model_and_shift_makes_it_pan() {
        assert_eq!(
            drag_for(Mode::Model, Button::Left, Mods::default()),
            Drag::Orbit
        );
        assert_eq!(
            drag_for(Mode::Model, Button::Left, Mods::shift()),
            Drag::Pan
        );
        assert_eq!(
            drag_for(Mode::Model, Button::Right, Mods::default()),
            Drag::Pan
        );
        assert_eq!(
            drag_for(Mode::Model, Button::Middle, Mods::default()),
            Drag::Pan
        );
    }

    #[test]
    fn a_2d_document_never_orbits() {
        for b in [Button::Left, Button::Right, Button::Middle] {
            for m in [Mods::default(), Mods::shift()] {
                assert_eq!(
                    drag_for(Mode::Canvas, b, m),
                    Drag::Pan,
                    "there is nothing to orbit in a flat document ({b:?}, {m:?})"
                );
            }
        }
    }

    #[test]
    fn the_documented_keys_are_bound_and_nothing_else_is() {
        assert_eq!(action_for("f"), Some(Action::Frame));
        assert_eq!(action_for("1"), Some(Action::Reset));
        assert_eq!(action_for("q"), Some(Action::Quit));
        assert_eq!(action_for("escape"), Some(Action::Quit));
        assert_eq!(action_for("2"), None);
        assert_eq!(action_for("F"), None, "the caller lowercases first");
        assert_eq!(action_for(""), None);
    }

    #[test]
    fn trackpad_and_wheel_scrolling_land_on_the_same_scale() {
        assert_eq!(notches(Wheel::Lines(1.0)), 1.0);
        assert_eq!(notches(Wheel::Pixels(PIXELS_PER_NOTCH)), 1.0);
        assert_eq!(notches(Wheel::Pixels(-PIXELS_PER_NOTCH * 2.0)), -2.0);
    }

    #[test]
    fn a_violent_trackpad_flick_is_clamped_to_a_usable_step() {
        assert_eq!(notches(Wheel::Pixels(100_000.0)), MAX_NOTCHES_PER_EVENT);
        assert_eq!(notches(Wheel::Lines(-999.0)), -MAX_NOTCHES_PER_EVENT);
    }
}
