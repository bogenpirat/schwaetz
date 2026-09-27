//! Small immediate-mode animations for button hover/press and switch transitions.
//!
//! While drawing, a control asks for the current value of one of its channels (hover, press,
//! switch position). The store moves that value toward its target by the time elapsed since the
//! control was last drawn and notes whether anything is still in motion, so the shell repaints
//! only while a transition runs and stays idle otherwise.

use crate::gfx::{Color, Painter, Rect, hex, with_alpha};
use crate::theme::Theme;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Instant;

/// A control with animated states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    SidebarStatus,
    SidebarSettings,
    /// The settings button of a network row (server buffer id).
    NetworkSettings(u32),
    /// Save / Cancel / Remove at the bottom of a dialog (by position).
    DialogFooter(usize),
    /// A button or switch field in a dialog (by field index).
    DialogField(usize),
    ConfirmYes,
    ConfirmNo,
    /// The Reply button of a chat line.
    Reply(u64),
    ReplyClose,
    JumpPill,
    /// "Open stream" in the topic bar of Twitch channels.
    OpenStream,
}

impl Control {
    fn in_dialog(self) -> bool {
        matches!(self, Control::DialogFooter(_) | Control::DialogField(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Channel {
    Hover,
    Press,
    Switch,
}

struct Value {
    current: f32,
    target: f32,
    at: Instant,
}

/// Durations in seconds for going up (toward 1) and down.
const HOVER: (f32, f32) = (0.12, 0.18);
const PRESS: (f32, f32) = (0.06, 0.16);
const SWITCH: (f32, f32) = (0.14, 0.14);
/// A transition that starts now advances by about one frame on its first draw.
const FIRST_STEP: f32 = 1.0 / 60.0;

#[derive(Default)]
pub struct Anims {
    values: RefCell<HashMap<(Control, Channel), Value>>,
    moving: Cell<bool>,
    /// The control under the pointer.
    pub hot: Option<Control>,
    /// The control under a pressed left button.
    pub pressed: Option<Control>,
}

impl Anims {
    /// Call before drawing a frame.
    pub fn begin_frame(&self) {
        self.moving.set(false);
    }

    /// Whether a transition was still running in the last frame (so another one is needed).
    pub fn moving(&self) -> bool {
        self.moving.get()
    }

    /// Hover amount of `c`, eased, 0..1.
    pub fn hover(&self, c: Control) -> f32 {
        self.value((c, Channel::Hover), if self.hot == Some(c) { 1.0 } else { 0.0 }, HOVER, 0.0)
    }

    /// Press amount of `c`, eased, 0..1.
    pub fn press(&self, c: Control) -> f32 {
        self.value((c, Channel::Press), if self.pressed == Some(c) { 1.0 } else { 0.0 }, PRESS, 0.0)
    }

    /// Knob position of a switch (0 off, 1 on); starts where it is when first drawn.
    pub fn switch(&self, c: Control, on: bool) -> f32 {
        let target = if on { 1.0 } else { 0.0 };
        self.value((c, Channel::Switch), target, SWITCH, target)
    }

    /// Forgets dialog controls (a dialog closed or its rows changed), so a new dialog does not
    /// animate from the previous one's states.
    pub fn forget_dialog(&self) {
        self.values.borrow_mut().retain(|(c, _), _| !c.in_dialog());
    }

    fn value(&self, key: (Control, Channel), target: f32, (up, down): (f32, f32), initial: f32) -> f32 {
        let now = Instant::now();
        let mut values = self.values.borrow_mut();
        let v = values.entry(key).or_insert(Value { current: initial, target: initial, at: now });
        if v.target != target {
            // A new transition: measure from about one frame ago, however long we were idle.
            v.target = target;
            v.at = now - std::time::Duration::from_secs_f32(FIRST_STEP);
        }
        let dt = now.duration_since(v.at).as_secs_f32();
        v.at = now;
        if v.current != target {
            let step = dt / if target > v.current { up } else { down };
            v.current =
                if target > v.current { (v.current + step).min(target) } else { (v.current - step).max(target) };
            if v.current != target {
                self.moving.set(true);
            }
        }
        let t = v.current;
        // Settled idle hover/press values need no memory.
        if t == 0.0 && target == 0.0 && key.1 != Channel::Switch {
            values.remove(&key);
        }
        smoothstep(t)
    }
}

/// Draws a push button's face with hover and press feedback and returns the rect its label goes
/// in (pressed buttons shrink a little).
pub fn button_face(p: &Painter, r: Rect, radius: f32, fill: Color, th: &Theme, hover: f32, press: f32) -> Rect {
    let r = r.inset(press * 1.5, press * 1.5);
    p.fill_round(r, radius, fill);
    if hover > 0.0 {
        // Toward the text color: lighter on dark themes, darker on light ones.
        p.fill_round(r, radius, with_alpha(th.text, 0.10 * hover));
    }
    if press > 0.0 {
        p.fill_round(r, radius, with_alpha(hex(0x000000), 0.14 * press));
    }
    r
}

/// Linear blend of two colors.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Color { r: l(a.r, b.r), g: l(a.g, b.g), b: l(a.b, b.b), a: l(a.a, b.a) }
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_run_and_settle() {
        let mut a = Anims::default();
        a.begin_frame();
        assert_eq!(a.hover(Control::SidebarSettings), 0.0);
        assert!(!a.moving());
        a.hot = Some(Control::SidebarSettings);
        a.begin_frame();
        let first = a.hover(Control::SidebarSettings);
        assert!(first > 0.0 && first < 1.0, "starts moving: {first}");
        assert!(a.moving());
        std::thread::sleep(std::time::Duration::from_millis(250));
        a.begin_frame();
        assert_eq!(a.hover(Control::SidebarSettings), 1.0);
        assert!(!a.moving());
        // Switches start where they are, then slide.
        assert_eq!(a.switch(Control::DialogField(3), true), 1.0);
        a.begin_frame();
        let s = a.switch(Control::DialogField(3), false);
        assert!(s < 1.0 && s > 0.0);
        a.forget_dialog();
        assert_eq!(a.switch(Control::DialogField(3), false), 0.0);
    }
}
