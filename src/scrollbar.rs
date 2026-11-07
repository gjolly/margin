// SPDX-License-Identifier: GPL-3.0-or-later
//! App-side scrollbars for independently scrolling viewports.
use std::cell::Cell;
use std::rc::Rc;

use rune::{Cursor, El, Live, Phase, Runtime, col, el};
use rune_kit::Kit;

const VIEWPORT: &str = "review-diff/scroll";
const WIDTH: f32 = 14.0;
const INSET: f32 = 2.0;

#[derive(Clone, Copy, Default)]
struct Position {
    height: f32,
    max: f32,
    offset: f32,
}

impl Position {
    fn thumb(self) -> (f32, f32) {
        let track = (self.height - 2.0 * INSET).max(0.0);
        let length = (track * self.height / (self.height + self.max).max(1.0))
            .max(24.0).min(track);
        let top = if self.max > 0.0 {
            (track - length) * (self.offset / self.max).clamp(0.0, 1.0)
        } else { 0.0 };
        (INSET + top, length)
    }

    fn offset_at(self, top: f32) -> f32 {
        let travel = self.height - 2.0 * INSET - self.thumb().1;
        if travel <= 0.0 { return self.offset; }
        ((top - INSET) / travel * self.max).clamp(0.0, self.max)
    }
}

#[derive(Clone)]
pub struct Scrollbar {
    viewport: &'static str,
    scroll_key: &'static str,
    area_key: &'static str,
    bar_key: &'static str,
    left: bool,
    position: Rc<Cell<Position>>,
    rect: Live<[f32; 4]>,
    thumb: Live<[f32; 4]>,
}

impl Scrollbar {
    pub fn new() -> Self {
        Self::for_viewport(VIEWPORT, "scroll", "diff-area", "diff-scrollbar")
    }

    pub fn for_viewport(viewport: &'static str, scroll_key: &'static str, area_key: &'static str, bar_key: &'static str) -> Self {
        Self {
            viewport, scroll_key, area_key, bar_key, left: false,
            position: Rc::new(Cell::new(Position::default())),
            rect: Live::new([0.0; 4]),
            thumb: Live::new([0.0; 4]),
        }
    }

    pub fn on_left(mut self) -> Self {
        self.left = true;
        self
    }

    /// Runs only after a drawn frame. `presented` reads geometry without subscribing
    /// the view to changing scroll offsets. Each viewport has a unique element key.
    /// Live values request a frame only when their geometry actually changes.
    pub fn follow<S: 'static>(&self, rt: &Runtime<S>, zoom: f32) {
        // Accessibility exposes the viewport's actual scroll range, including growth
        // animation and virtual spacers. The list's layout height alone does not.
        let tree = rt.accessibility_tree();
        let scroll = tree.nodes.iter().find_map(|(_, node)| {
            if node.author_id() != Some(self.scroll_key) { return None; }
            Some((node.scroll_y()? as f32, node.scroll_y_max()? as f32))
        });
        let geometry = rt.presented(self.scroll_key).zip(scroll);
        let Some((([_, _, width, height], _), (offset, max))) = geometry else {
            self.rect.snap([0.0; 4]);
            self.position.set(Position::default());
            return;
        };
        let (width, height) = (width / zoom, height / zoom);
        let offset = offset.clamp(0.0, max);
        let position = Position { height, max, offset };
        self.position.set(position);
        let (top, length) = position.thumb();
        self.rect.snap([if self.left { 0.0 } else { width }, 0.0, WIDTH, if max > 0.5 { height } else { 0.0 }]);
        self.thumb.snap([4.0, top, 6.0, length]);
    }

    pub fn build(&self, content: El, k: &Kit) -> El {
        let position = self.position.clone();
        let viewport = self.viewport;
        let content = if self.left { content.pl(WIDTH) } else { content.pr(WIDTH) };
        col().key(self.area_key).w_full().grow(1.0).min_h(0.0)
            .child(content)
            .child(el().key(self.bar_key).absolute(0.0, 0.0)
                .live_rect(&self.rect).fill(k.c.layer_01).cursor(Cursor::Pointer)
                .a11y_hidden()
                .on_drag_any(move |drag, tx| {
                    tx.live_only();
                    let at = position.get();
                    if at.max <= 0.5 || drag.phase == Phase::Up { return; }
                    let (top, length) = at.thumb();
                    if drag.phase == Phase::Down {
                        // Hold the thumb where it was grabbed; a track click centres it.
                        drag.data = if (top..=top + length).contains(&drag.y) {
                            drag.y - top
                        } else { length / 2.0 };
                    }
                    tx.scroll_to_y(viewport, at.offset_at(drag.y - drag.data));
                })
                .child(el().key("thumb").absolute(0.0, 0.0).live_rect(&self.thumb)
                    .radius(3.0).fill(k.c.text_muted)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Cx, Env};
    use crate::diff::{DiffState, DiffView, parse};

    struct App {
        diff: DiffState,
        bar: Scrollbar,
        zoom: f32,
        builds: Cell<usize>,
    }

    fn view(s: &App, _: &mut Cx<App>) -> El {
        s.builds.set(s.builds.get() + 1);
        let k = Kit::neutral(true);
        let diff = DiffView::new(|s: &App| &s.diff, |s: &mut App| &mut s.diff)
            .key("review-diff").build(s, &k);
        col().w_full().h_full().child(
            col().size(600.0 / s.zoom, 400.0 / s.zoom).absolute(0.0, 0.0)
                .scale(s.zoom).origin(0.0, 0.0).child(s.bar.build(diff, &k)))
    }

    fn tick(rt: &mut Runtime<App>) {
        rt.frame(1.0 / 60.0);
        rt.state.bar.follow(rt, rt.state.zoom);
    }

    #[test]
    fn follows_virtual_diff_and_track_click_scrolls_at_each_zoom() {
        for zoom in [0.5, 1.0, 2.0] {
            let mut patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,200 +1,200 @@\n".to_owned();
            for _ in 0..200 { patch.push_str("-old\n+new\n"); }
            let app = App { diff: DiffState::new(parse(&patch)), bar: Scrollbar::new(), zoom, builds: Cell::new(0) };
            let mut rt = Runtime::new(app, view, (600.0, 400.0), Env::new(true, 1.0));
            for _ in 0..90 { tick(&mut rt); }
            let p = rt.state.bar.position.get();
            assert!(p.max > 0.0);
            assert!((p.height - 400.0 / zoom).abs() < 1.0, "height {} at zoom {zoom}", p.height);
            rt.wheel(200.0, 200.0, -300.0);
            for _ in 0..90 { tick(&mut rt); }
            assert!(rt.state.bar.position.get().offset > 0.0);
            rt.wheel(200.0, 200.0, -100.0);
            for _ in 0..90 { tick(&mut rt); }
            let (x, y) = rt.find("diff-scrollbar").unwrap();
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            for _ in 0..90 { tick(&mut rt); }
            let p = rt.state.bar.position.get();
            assert!(p.offset > p.max * 0.3 && p.offset < p.max * 0.7);
            let (x, y) = rt.find("diff-scrollbar/thumb").unwrap();
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            tick(&mut rt);
            // Newly mounted virtual rows refine the estimated scroll extent.
            for _ in 0..90 {
                rt.pointer_move(x, 399.0);
                tick(&mut rt);
            }
            rt.pointer_up(x, 399.0);
            for _ in 0..90 { tick(&mut rt); }
            let p = rt.state.bar.position.get();
            assert!((p.offset - p.max).abs() < 1.0, "offset {}, max {}, zoom {zoom}", p.offset, p.max);
        }
    }

    #[test]
    fn following_geometry_adds_no_view_rebuilds_to_wheel_scrolling() {
        let run = |follow| {
            let mut patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,200 +1,200 @@\n".to_owned();
            for _ in 0..200 { patch.push_str("-old\n+new\n"); }
            let app = App { diff: DiffState::new(parse(&patch)), bar: Scrollbar::new(), zoom: 1.0, builds: Cell::new(0) };
            let mut rt = Runtime::new(app, view, (600.0, 400.0), Env::new(true, 1.0));
            let mut counts = Vec::new();
            for frame in 0..30 {
                if frame == 10 || frame == 20 { rt.wheel(200.0, 200.0, -300.0); }
                rt.frame(1.0 / 60.0);
                if follow { rt.state.bar.follow(&rt, 1.0); }
                counts.push(rt.state.builds.get());
            }
            counts
        };
        assert_eq!(run(true), run(false));
    }

    #[test]
    fn thumb_tracks_scroll_extent_and_drag_reaches_both_ends() {
        let mut p = Position { height: 500.0, max: 1500.0, offset: 0.0 };
        let (top, length) = p.thumb();
        assert_eq!(top, INSET);
        assert_eq!(length, 124.0);
        p.offset = p.max;
        let (bottom, _) = p.thumb();
        assert_eq!(bottom + length, p.height - INSET);
        assert_eq!(p.offset_at(bottom), p.max);
        assert_eq!(p.offset_at(-100.0), 0.0);
        assert_eq!(p.offset_at(1000.0), p.max);
    }

    #[test]
    fn small_viewports_and_long_diffs_keep_thumb_bounded() {
        for height in [0.0, 10.0, 500.0] {
            let p = Position { height, max: 1_000_000.0, offset: 0.0 };
            let (_, length) = p.thumb();
            assert!(length >= 0.0 && length <= (height - 4.0).max(0.0));
            assert!(p.offset_at(100.0).is_finite());
        }
    }
}
