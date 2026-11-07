// SPDX-License-Identifier: GPL-3.0-or-later
//! App-side scrollbars for independently scrolling viewports.
use std::cell::Cell;
use std::rc::Rc;

use rune::{Cursor, El, Live, Phase, ScrollPos, col, el};
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
    area_key: &'static str,
    bar_key: &'static str,
    left: bool,
    position: Rc<Cell<Position>>,
    rect: Live<[f32; 4]>,
    thumb: Live<[f32; 4]>,
}

impl Scrollbar {
    pub fn new() -> Self {
        Self::for_viewport(VIEWPORT, "diff-area", "diff-scrollbar")
    }

    pub fn for_viewport(viewport: &'static str, area_key: &'static str, bar_key: &'static str) -> Self {
        Self {
            viewport, area_key, bar_key, left: false,
            position: Rc::new(Cell::new(Position::default())),
            rect: Live::new([0.0; 4]),
            thumb: Live::new([0.0; 4]),
        }
    }

    pub fn on_left(mut self) -> Self {
        self.left = true;
        self
    }

    /// Update live geometry before the viewport's frame draws. ScrollPos uses the
    /// viewport's local points, so zoom needs no conversion or layout subscription.
    pub fn on_scroll_offset(&self) -> impl Fn(ScrollPos) + 'static {
        let bar = self.clone();
        move |at| {
            let [width, height] = at.drawn;
            let max = at.max[1].max(0.0);
            let position = Position { height, max, offset: at.offset[1].clamp(0.0, max) };
            bar.position.set(position);
            let (top, length) = position.thumb();
            bar.rect.snap([if bar.left { 0.0 } else { width }, 0.0, WIDTH, if max > 0.5 { height } else { 0.0 }]);
            bar.thumb.snap([4.0, top, 6.0, length]);
        }
    }

    pub fn build(&self, content: El, k: &Kit) -> El {
        let position = self.position.clone();
        let viewport = self.viewport;
        // Reserve the gutter outside the content: padding the diff itself lets its
        // absolute sticky header span the padding and extend under the scrollbar.
        let gutter = col().w_full().grow(1.0).min_h(0.0);
        let content = if self.left { gutter.pl(WIDTH) } else { gutter.pr(WIDTH) }.child(content);
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
    use rune::{Cx, Env, Runtime};
    use crate::diff::{DiffState, DiffView, parse};

    struct App {
        diff: DiffState,
        bar: Scrollbar,
        zoom: f32,
        builds: Cell<usize>,
        follow: bool,
    }

    fn view(s: &App, _: &mut Cx<App>) -> El {
        s.builds.set(s.builds.get() + 1);
        let k = Kit::neutral(true);
        let mut diff = DiffView::new(|s: &App| &s.diff, |s: &mut App| &mut s.diff)
            .key("review-diff");
        if s.follow { diff = diff.on_scroll_offset(s.bar.on_scroll_offset()); }
        let diff = diff.build(s, &k);
        col().w_full().h_full().child(
            col().size(600.0 / s.zoom, 400.0 / s.zoom).absolute(0.0, 0.0)
                .scale(s.zoom).origin(0.0, 0.0).child(s.bar.build(diff, &k)))
    }

    fn tick(rt: &mut Runtime<App>) {
        rt.frame(1.0 / 60.0);
    }

    #[test]
    fn thumb_matches_current_scroll_frame_through_motion_and_zoom() {
        for zoom in [0.5, 1.0, 2.0] {
            let mut patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,200 +1,200 @@\n".to_owned();
            for _ in 0..200 { patch.push_str("-old\n+new\n"); }
            let app = App { diff: DiffState::new(parse(&patch)), bar: Scrollbar::new(), zoom,
                builds: Cell::new(0), follow: true };
            let mut rt = Runtime::new(app, view, (600.0, 400.0), Env::new(true, 1.0));
            for _ in 0..45 { tick(&mut rt); }
            rt.wheel(200.0, 200.0, -300.0);
            for frame in 0..45 {
                if frame == 15 {
                    rt.update(|s, tx| { tx.instant(); s.zoom = 1.25; });
                }
                tick(&mut rt);
                let tree = rt.accessibility_tree();
                let scroll = &tree.nodes.iter().find(|(_, n)| n.author_id() == Some("scroll")).unwrap().1;
                let position = rt.state.bar.position.get();
                assert!((position.offset - scroll.scroll_y().unwrap() as f32).abs() < 0.1,
                    "position lags at frame {frame}, zoom {zoom}");
                assert!((position.max - scroll.scroll_y_max().unwrap() as f32).abs() < 0.1);
                let ([_, bar_y, _, bar_h], _) = rt.presented("diff-scrollbar").unwrap();
                let ([_, thumb_y, _, thumb_h], _) = rt.elements().iter().find(|e| e.path.ends_with("/diff-scrollbar/thumb")).map(|e| (e.rect, e.opacity)).unwrap();
                let (top, length) = position.thumb();
                let scale = rt.state.zoom;
                assert!((bar_h / scale - position.height).abs() < 1.0);
                assert!(((thumb_y - bar_y) / scale - top).abs() < 1.0,
                    "drawn thumb lags at frame {frame}, zoom {zoom}");
                assert!((thumb_h / scale - length).abs() < 1.0);
            }
        }
    }

    #[test]
    fn sticky_file_counts_align_with_list_counts_outside_scrollbar() {
        for zoom in [0.5, 1.0, 2.0] {
            for mode in [crate::diff::DiffMode::Split, crate::diff::DiffMode::Unified] {
                let mut patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,200 +1,200 @@\n".to_owned();
                for _ in 0..200 { patch.push_str("-old\n+new\n"); }
                let mut diff = DiffState::new(parse(&patch));
                diff.set_mode(mode);
                let app = App { diff, bar: Scrollbar::new(), zoom, builds: Cell::new(0), follow: true };
                let mut rt = Runtime::new(app, view, (600.0, 400.0), Env::new(true, 1.0));
                for _ in 0..90 { tick(&mut rt); }
                let (list_x, _) = rt.find("df0/inner/dels").expect("list deletion count");
                rt.wheel(200.0, 200.0, -100.0);
                for _ in 0..90 { tick(&mut rt); }
                let (sticky_x, _) = rt.find("sticky/inner/dels").expect("sticky deletion count");
                let ([sticky_left, _, sticky_width, _], _) = rt.presented("sticky").unwrap();
                let ([bar_x, _, _, _], _) = rt.presented("diff-scrollbar").unwrap();
                assert!((sticky_x - list_x).abs() < 1.0,
                    "list {list_x}, sticky {sticky_x}, zoom {zoom}, mode {mode:?}");
                assert!((sticky_left + sticky_width - bar_x).abs() < 1.0,
                    "sticky edge {}, scrollbar {bar_x}, zoom {zoom}", sticky_left + sticky_width);
            }
        }
    }

    #[test]
    fn follows_virtual_diff_and_track_click_scrolls_at_each_zoom() {
        for zoom in [0.5, 1.0, 2.0] {
            let mut patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,200 +1,200 @@\n".to_owned();
            for _ in 0..200 { patch.push_str("-old\n+new\n"); }
            let app = App { diff: DiffState::new(parse(&patch)), bar: Scrollbar::new(), zoom, builds: Cell::new(0), follow: true };
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
            let app = App { diff: DiffState::new(parse(&patch)), bar: Scrollbar::new(), zoom: 1.0, builds: Cell::new(0), follow };
            let mut rt = Runtime::new(app, view, (600.0, 400.0), Env::new(true, 1.0));
            let mut counts = Vec::new();
            for frame in 0..30 {
                if frame == 10 || frame == 20 { rt.wheel(200.0, 200.0, -300.0); }
                rt.frame(1.0 / 60.0);
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
