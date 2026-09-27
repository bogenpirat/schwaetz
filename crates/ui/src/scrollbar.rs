//! Vertical scrollbar at the right edge of a pane: a thin thumb that widens under the pointer and
//! can be dragged, or clicked beside to jump there.

use crate::gfx::{Painter, Rect, with_alpha};
use crate::theme::Theme;

/// Width of the strip at the pane's right edge that belongs to the scrollbar.
const HIT_W: f32 = 14.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scrollbar {
    /// Where the thumb can move (full height of the strip).
    pub track: Rect,
    pub thumb: Rect,
}

impl Scrollbar {
    /// A scrollbar along the right edge of `area`: `ratio` is the visible part of the content
    /// (sets the thumb's length), `pos` the scroll position from 0 (top) to 1 (bottom).
    pub fn new(area: Rect, ratio: f32, pos: f32) -> Scrollbar {
        let track = Rect::new(area.right() - HIT_W, area.y + 6.0, HIT_W, (area.h - 12.0).max(0.0));
        let h = (track.h * ratio.clamp(0.0, 1.0)).max(28.0).min(track.h);
        let thumb = Rect::new(track.x, track.y + (track.h - h) * pos.clamp(0.0, 1.0), HIT_W, h);
        Scrollbar { track, thumb }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.track.contains(x, y)
    }

    /// Where the pointer holds the thumb when a drag starts at `y`: the offset into the thumb,
    /// or its middle when the track beside it was clicked (the thumb jumps there).
    pub fn grab(&self, y: f32) -> f32 {
        if y >= self.thumb.y && y < self.thumb.bottom() { y - self.thumb.y } else { self.thumb.h / 2.0 }
    }

    /// Scroll position (0 = top, 1 = bottom) for the pointer at `y` holding the thumb at `grab`.
    pub fn pos_at(&self, y: f32, grab: f32) -> f32 {
        let room = (self.track.h - self.thumb.h).max(1.0);
        ((y - grab - self.track.y) / room).clamp(0.0, 1.0)
    }

    /// Draws the thumb: thin at rest, wider with a faint track while `hot` (hovered or dragged).
    pub fn draw(&self, p: &Painter, th: &Theme, hot: bool) {
        let w = if hot { 8.0 } else { 4.0 };
        let x = self.track.right() - 3.0 - w;
        if hot {
            let track = Rect::new(x, self.track.y, w, self.track.h);
            p.fill_round(track, w / 2.0, with_alpha(th.scrollbar, th.scrollbar.a * 0.35));
        }
        p.fill_round(Rect::new(x, self.thumb.y, w, self.thumb.h), w / 2.0, th.scrollbar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip() {
        let area = Rect::new(0.0, 0.0, 300.0, 412.0);
        let bar = Scrollbar::new(area, 0.25, 0.5);
        assert_eq!(bar.thumb.h, 100.0);
        assert_eq!(bar.thumb.y, 6.0 + 150.0);
        // Dragging the thumb by its middle to where it is keeps the position.
        let grab = bar.grab(bar.thumb.y + 50.0);
        assert_eq!(grab, 50.0);
        assert_eq!(bar.pos_at(bar.thumb.y + 50.0, grab), 0.5);
        // Clicking the track beside the thumb centers it there; the ends clamp.
        assert_eq!(bar.grab(10.0), 50.0);
        assert_eq!(bar.pos_at(0.0, 50.0), 0.0);
        assert_eq!(bar.pos_at(1000.0, 50.0), 1.0);
        // Tiny ratios still leave a grabbable thumb.
        assert_eq!(Scrollbar::new(area, 0.001, 0.0).thumb.h, 28.0);
    }
}
