// SPDX-License-Identifier: GPL-3.0-or-later
//! Cached, always-expanded changed-file outline and its resizable pane.
use std::{borrow::Cow, collections::BTreeMap};

use rune::{Cursor, El, GrowthSpec, Phase, ScrollSpec, Tx, col, el, row, text};
use rune_kit::{Button, Kit, natural_cmp};
use crate::diff::{FileDiff, FileStatus};

use crate::{Review, scrollbar::Scrollbar};

const ROW_HEIGHT: f32 = 28.0;
const GRIP: f32 = 6.0;

struct Entry {
    key: String,
    name: String,
    path: String,
    depth: usize,
    file: Option<usize>,
    status: Option<FileStatus>,
}

pub struct FilePane {
    revision: u64,
    rows: Vec<Entry>,
    file_rows: Vec<usize>,
    pub visible: bool,
    pub width: f32,
    active: Option<usize>,
    pub scrollbar: Scrollbar,
}

impl FilePane {
    pub fn new() -> Self {
        Self { revision: 0, rows: Vec::new(), file_rows: Vec::new(), visible: true, width: 260.0,
            active: None, scrollbar: Scrollbar::for_viewport(
                "tree-scroll", "tree-scroll", "tree-area", "tree-scrollbar").on_left() }
    }

    /// Build the outline and return the diff files in the same traversal order.
    /// Reindex before creating DiffState so navigation, chat anchors and context agree.
    pub fn load(&mut self, files: Vec<FileDiff>) -> Vec<FileDiff> {
        // Separate directory and file identities also represent deleting `a` while adding `a/b`.
        #[derive(Default)]
        struct Dir { dirs: BTreeMap<String, Dir>, files: Vec<(String, String, usize, FileStatus)> }
        let mut root = Dir::default();
        for (index, file) in files.iter().enumerate() {
            let path = file.new_path.as_ref().or(file.old_path.as_ref()).map(String::as_str).unwrap_or("(unknown)");
            let mut parts: Vec<_> = path.split('/').collect();
            let name = parts.pop().unwrap_or(path);
            let mut dir = &mut root;
            for part in parts { dir = dir.dirs.entry(part.to_owned()).or_default(); }
            dir.files.push((name.to_owned(), path.to_owned(), index, file.status));
        }
        fn flatten(dir: Dir, prefix: &str, depth: usize, rows: &mut Vec<Entry>, file_rows: &mut [usize]) {
            let mut dirs: Vec<_> = dir.dirs.into_iter().collect();
            dirs.sort_by(|a, b| natural_cmp(&a.0, &b.0));
            for (name, child) in dirs {
                let path = format!("{prefix}{name}");
                rows.push(Entry { key: format!("dir:{path}"), name, path: path.clone(), depth, file: None, status: None });
                flatten(child, &format!("{path}/"), depth + 1, rows, file_rows);
            }
            let mut files = dir.files;
            files.sort_by(|a, b| natural_cmp(&a.0, &b.0));
            for (name, path, index, status) in files {
                file_rows[index] = rows.len();
                rows.push(Entry { key: format!("file:{index}"), name, path, depth, file: Some(index), status: Some(status) });
            }
        }
        self.revision = self.revision.wrapping_add(1);
        self.rows.clear();
        self.file_rows = vec![0; files.len()];
        flatten(root, "", 0, &mut self.rows, &mut self.file_rows);
        // Move patches rather than cloning their potentially large hunk contents.
        let mut original: Vec<_> = files.into_iter().map(Some).collect();
        let mut ordered = Vec::with_capacity(original.len());
        for (row, entry) in self.rows.iter_mut().enumerate() {
            if let Some(old) = entry.file {
                let index = ordered.len();
                ordered.push(original[old].take().expect("each file occurs once in the outline"));
                entry.file = Some(index);
                entry.key = format!("file:{index}");
                self.file_rows[index] = row;
            }
        }
        self.active = (!ordered.is_empty()).then_some(0);
        ordered
    }

    pub fn follow(&mut self, file: usize) {
        self.active = (file < self.file_rows.len()).then_some(file);
    }

    fn bounds(available: f32) -> (f32, f32) {
        let max = (available - 320.0 - GRIP).clamp(0.0, 480.0);
        (160.0_f32.min(max), max)
    }

    fn shown_width(&self, available: f32) -> f32 {
        if !self.visible { return 0.0; }
        let (min, max) = Self::bounds(available);
        self.width.clamp(min, max)
    }

    fn reveal(&self, tx: &mut Tx) {
        if self.visible && let Some(row) = self.active.and_then(|f| self.file_rows.get(f)) {
            tx.scroll_into_view_in("tree-scroll", self.rows[*row].key.clone());
        }
    }
}

fn status(status: FileStatus) -> (&'static str, &'static str) {
    match status {
        FileStatus::Modified => ("M", "Modified"),
        FileStatus::Added => ("A", "Added"),
        FileStatus::Deleted => ("D", "Deleted"),
        FileStatus::Renamed => ("R", "Renamed"),
        FileStatus::Copied => ("C", "Copied"),
    }
}

pub fn toggle(s: &mut Review, tx: &mut Tx) {
    tx.instant();
    s.files.visible = !s.files.visible;
    s.files.follow(s.diff.visible_file());
}

pub fn toggle_button(s: &Review, k: &Kit) -> El {
    Button::new(if s.files.visible { "Hide files" } else { "Show files" })
        .on_press_tx(toggle).build(k).key("toggle-files").expanded(s.files.visible)
}

pub fn build(s: &Review, k: &Kit, available: f32) -> El {
    let width = s.files.shown_width(available);
    let kit = k.clone();
    let rows = col().key("tree-rows").w_full().growth(GrowthSpec::default())
        .virtual_rows(s.files.rows.len(), ROW_HEIGHT,
            |s: &Review, i| Cow::Borrowed(s.files.rows[i].key.as_str()),
            move |s: &Review, i| {
                let entry = &s.files.rows[i];
                let selected = entry.file.is_some() && entry.file == s.files.active;
                let mut line = row().key(entry.key.clone()).h(ROW_HEIGHT).w_full().items_center()
                    .pl(10.0 + entry.depth as f32 * 14.0).pr(8.0).gap(6.0)
                    .fill(if selected { kit.alpha(kit.c.text_accent, 0.2) } else { kit.c.layer_01 });
                if let (Some(file), Some(change)) = (entry.file, entry.status) {
                    let (letter, label) = status(change);
                    let color = match change {
                        FileStatus::Added | FileStatus::Copied => kit.c.success.fg,
                        FileStatus::Deleted => kit.c.danger.fg,
                        _ => kit.c.text_accent,
                    };
                    line = line.child(text(letter).key("status").w(14.0).font(kit.ty.ui_medium).color(color))
                        .role("button").label(format!("{}, {label}", entry.path)).help(format!("{} — {label}", entry.path))
                        .focusable(true).focus_selected(selected).cursor(Cursor::Pointer)
                        .on_press_tx(move |s: &mut Review, tx| {
                            tx.instant();
                            s.diff.scroll_to_file(file, tx);
                            s.files.follow(file);
                        });
                    if !selected { line = line.hover(kit.c.layer_01.mix(kit.c.text_base, if kit.dark { 0.16 } else { 0.10 })); }
                } else {
                    line = line.child(text("▾").w(14.0).font(kit.ty.ui).color(kit.c.text_muted))
                        .label(format!("Folder {}", entry.path));
                }
                line.child(text(entry.name.clone()).key("name").font(kit.ty.ui).color(kit.c.text_base)
                    .truncate().grow(1.0).min_w(0.0))
            }).virtual_rows_version(s.files.revision);
    let viewport = col().key("tree-scroll").w_full().grow(1.0).min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false }).child(if s.files.rows.is_empty() {
            col().w_full().padding(10.0).child(text(s.message.clone().unwrap_or_default())
                .font(k.ty.ui).color(k.c.text_muted).wrap())
        } else { rows });
    let mut pane = col().key("file-pane").w(width).min_w(0.0).h_full().clip_soft(0.0).fill(k.c.layer_01)
        .child(text("Changed files").font(k.ty.ui_medium).color(k.c.text_muted).padding(10.0))
        .child(s.files.scrollbar.build(col().w_full().grow(1.0).min_h(0.0).child(viewport), k));
    // A new key reports layout once when the active file or visibility changes. This
    // issues the reveal after layout, without subscribing the app to scroll offsets.
    pane = pane.child(el().key(format!("follow-{:?}-{}", s.files.active, s.files.visible))
        .size(0.0, 0.0).absolute(0.0, 0.0).a11y_hidden()
        .on_layout_tx(|s: &mut Review, _, tx| { tx.live_only(); s.files.reveal(tx); }));
    if !s.files.visible { pane = pane.a11y_hidden(); }
    let (min, max) = FilePane::bounds(available);
    let mut grip = el().key("file-pane-grip").w(if s.files.visible { GRIP } else { 0.0 }).h_full()
        .cursor(Cursor::ResizeColumn).role("separator").label("Resize files pane")
        .range_value(width as f64, min as f64, max as f64).range_step(8.0)
        .fill(k.c.bg_base).child(el().absolute(2.0, 0.0).w(1.0).h_full().fill(k.c.text_muted))
        .on_step(move |s: &mut Review, steps, tx| { tx.instant(); s.files.width = (s.files.width + steps as f32 * 8.0).clamp(min, max); })
        .on_drag(move |s: &mut Review, drag, tx| {
            tx.instant();
            match drag.phase {
                Phase::Down => drag.data = s.files.width,
                Phase::Move => s.files.width = (drag.data.clamp(min, max) + drag.dx / s.ui_scale()).clamp(min, max),
                Phase::Up if drag.cancelled() => s.files.width = drag.data,
                Phase::Up => {},
            }
        });
    if !s.files.visible { grip = grip.a11y_hidden(); }
    row().key("file-pane-slot").h_full().min_w(0.0).child(pane).child(grip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Key, KeyEvent, Runtime};
    use crate::diff::{DiffMode, LineRange, LineRef, Side, parse};

    fn patch(count: usize, lines: usize) -> String {
        let mut patch = String::new();
        for file in 0..count {
            patch.push_str(&format!("diff --git a/src/file{file}.rs b/src/file{file}.rs\n--- a/src/file{file}.rs\n+++ b/src/file{file}.rs\n@@ -1,{lines} +1,{lines} @@\n"));
            for _ in 0..lines { patch.push_str("-old\n+new\n"); }
        }
        patch
    }

    fn app(count: usize, lines: usize, zoom: u16) -> Runtime<Review> {
        let mut state = crate::init();
        state.zoom_percent = zoom;
        crate::load_result(&mut state, Ok(patch(count, lines)));
        let mut rt = Runtime::new(state, crate::view, (1100.0, 720.0), Env::new(true, 1.0));
        settle(&mut rt);
        rt
    }

    fn settle(rt: &mut Runtime<Review>) {
        for _ in 0..45 {
            rt.frame(1.0 / 60.0);
            let zoom = rt.state.ui_scale();
            rt.state.scrollbar.follow(rt, zoom);
            rt.state.files.scrollbar.follow(rt, zoom);
        }
    }

    fn click(rt: &mut Runtime<Review>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        settle(rt);
    }

    fn offset(rt: &Runtime<Review>, key: &str) -> f64 {
        rt.accessibility_tree().nodes.iter().find_map(|(_, node)|
            (node.author_id() == Some(key)).then(|| node.scroll_y()).flatten()).unwrap()
    }

    #[test]
    fn hierarchy_preserves_statuses_and_file_directory_collisions() {
        let mut files = parse(&patch(5, 1));
        for (file, (path, change)) in files.iter_mut().zip([
            ("a", FileStatus::Deleted), ("a/file10", FileStatus::Added),
            ("a/file2", FileStatus::Renamed), ("space name", FileStatus::Copied),
            ("binary", FileStatus::Modified),
        ]) {
            file.old_path = Some(path.into());
            file.new_path = (change != FileStatus::Deleted).then(|| path.into());
            file.status = change;
        }
        files[2].old_path = Some("previous-name".into());
        files[4].binary = true;
        let mut pane = FilePane::new();
        let files = pane.load(files);
        assert_eq!(pane.rows.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["a", "file2", "file10", "a", "binary", "space name"]);
        for (index, row) in pane.file_rows.iter().enumerate() {
            assert_eq!(pane.rows[*row].file, Some(index));
            assert_eq!(pane.rows[*row].status, Some(files[index].status));
        }
        assert_ne!(pane.rows[0].key, pane.rows[3].key);
        assert_eq!(status(files[2].status), ("D", "Deleted"));
        pane.load(Vec::new());
        assert!(pane.rows.is_empty());
        assert_eq!(pane.active, None);
    }

    #[test]
    fn diff_order_matches_tree_across_directories_and_root_files() {
        let paths = ["README.md", "src/file10.rs", "src/file2.rs", "src/nested/a.rs", "src/nested/z.rs"];
        let patch: String = paths.iter().map(|path| patch(1, 40).replace("src/file0.rs", path)).collect();
        let mut state = crate::init();
        crate::load_result(&mut state, Ok(patch));
        let expected = ["src/nested/a.rs", "src/nested/z.rs", "src/file2.rs", "src/file10.rs", "README.md"];
        assert_eq!(state.diff.files().iter().map(|f| f.new_path.as_deref().unwrap()).collect::<Vec<_>>(), expected);
        let entries: Vec<_> = state.files.rows.iter().filter(|e| e.file.is_some()).collect();
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry.file, Some(index));
            assert_eq!(entry.path, expected[index]);
        }
        let mut rt = Runtime::new(state, crate::view, (1100.0, 720.0), Env::new(true, 1.0));
        settle(&mut rt);
        for mode in [DiffMode::Split, DiffMode::Unified] {
            rt.update(|s, tx| { s.diff.set_mode(mode); s.diff.scroll_to_file(0, tx); });
            settle(&mut rt);
            for index in 1..expected.len() {
                rt.update(|s, tx| { crate::on_key(s, &KeyEvent::from(Key::Text("]".into())), tx); });
                settle(&mut rt);
                assert_eq!(rt.state.diff.visible_file(), index);
                assert_eq!(rt.state.files.active, Some(index));
            }
            click(&mut rt, "file:0");
            assert_eq!(rt.state.diff.visible_file(), 0);
        }
    }

    #[test]
    fn clicking_and_keyboard_navigation_keep_one_continuous_diff_and_threads() {
        let mut rt = app(4, 30, 100);
        let anchor = LineRange::line(0, LineRef::new(Side::New, 1));
        rt.update(|s, _| s.chats.open(&mut s.diff, anchor));
        settle(&mut rt);
        for mode in [DiffMode::Split, DiffMode::Unified] {
            rt.update(|s, _| s.diff.set_mode(mode));
            settle(&mut rt);
            click(&mut rt, "file:2");
            assert_eq!(rt.state.files.active, Some(2));
            assert_eq!(rt.state.diff.visible_file(), 2);
            assert_eq!(rt.state.diff.files().len(), 4);
            rt.update(|s, tx| { crate::on_key(s, &KeyEvent::from(Key::Text("[".into())), tx); });
            settle(&mut rt);
            assert_eq!(rt.state.files.active, Some(1));
            assert_eq!(rt.state.chats.threads.len(), 1);
        }
    }

    #[test]
    fn long_tree_scrolls_independently_and_follows_diff_without_fighting_browsing() {
        let mut rt = app(150, 8, 100);
        let (bar_x, _) = rt.find("tree-scrollbar").unwrap();
        let (row_x, _) = rt.find("file:0").unwrap();
        assert!(bar_x < 14.0 && bar_x < row_x, "tree scrollbar must precede the rows");
        let diff_before = offset(&rt, "scroll");
        rt.wheel(100.0, 300.0, -500.0);
        settle(&mut rt);
        let tree_after = offset(&rt, "tree-scroll");
        assert!(tree_after > 0.0);
        assert_eq!(offset(&rt, "scroll"), diff_before);
        // Scroll the diff to another file: the tree follows even beyond its mounted rows.
        rt.update(|s, tx| { s.diff.scroll_to_file(110, tx); });
        settle(&mut rt);
        assert_eq!(rt.state.files.active, Some(110));
        assert!(offset(&rt, "tree-scroll") > tree_after);
        let (_, row_y) = rt.find("file:110").unwrap();
        assert!((48.0..720.0).contains(&row_y), "active row at {row_y}");
        let current = offset(&rt, "scroll");
        rt.wheel(100.0, 300.0, 300.0);
        settle(&mut rt);
        assert_eq!(offset(&rt, "scroll"), current);
        let browse = offset(&rt, "tree-scroll");
        settle(&mut rt);
        assert_eq!(offset(&rt, "tree-scroll"), browse);
        let mounted = rt.elements().iter().filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with("file:"))).count();
        let height = rt.presented("tree-scroll").unwrap().0[3];
        let limit = 5 * (height / ROW_HEIGHT).ceil() as usize + 2;
        assert!(mounted <= limit && mounted < 150, "mounted {mounted}, limit {limit}");
        for _ in 0..5 {
            assert!(!rt.frame(1.0 / 60.0), "idle app requested another frame");
            rt.state.scrollbar.follow(&rt, 1.0);
            rt.state.files.scrollbar.follow(&rt, 1.0);
        }
        // Both custom scrollbars target their own viewport even when the tree comes first.
        click(&mut rt, "tree-scrollbar");
        assert_eq!(offset(&rt, "scroll"), current);
        click(&mut rt, "diff-scrollbar");
        assert_ne!(offset(&rt, "scroll"), current);
        rt.wheel(800.0, 300.0, -1000.0);
        settle(&mut rt);
        assert_eq!(rt.state.files.active, Some(rt.state.diff.visible_file()));
        click(&mut rt, "toggle-files");
        rt.update(|s, tx| s.diff.scroll_to_file(10, tx));
        settle(&mut rt);
        click(&mut rt, "toggle-files");
        let (_, row_y) = rt.find("file:10").unwrap();
        assert!((48.0..720.0).contains(&row_y), "reopened active row at {row_y}");
    }

    #[test]
    fn pane_toggle_and_drag_preserve_diff_state_at_every_zoom() {
        for zoom in [50, 100, 200] {
            let mut rt = app(10, 15, zoom);
            let scale = rt.state.ui_scale();
            let (x, y) = rt.find("file-pane-grip").unwrap();
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            rt.pointer_move(x + 40.0 * scale, y);
            settle(&mut rt);
            rt.pointer_up(x + 40.0 * scale, y);
            settle(&mut rt);
            let max = FilePane::bounds(1100.0 / scale).1;
            assert!((rt.state.files.width - 300.0_f32.min(max)).abs() < 1.0,
                "width {} at zoom {zoom}", rt.state.files.width);
            let width = rt.state.files.width;
            click(&mut rt, "toggle-files");
            assert!(!rt.state.files.visible);
            rt.update(|s, tx| { s.diff.scroll_to_file(4, tx); });
            settle(&mut rt);
            click(&mut rt, "toggle-files");
            assert!(rt.state.files.visible);
            assert_eq!(rt.state.files.width, width);
            assert_eq!(rt.state.files.active, Some(4));
            assert_eq!(rt.state.diff.files().len(), 10);
        }
    }

    #[test]
    fn narrow_windows_and_cancelled_drag_stay_bounded() {
        let mut rt = app(5, 3, 100);
        let (x, y) = rt.find("file-pane-grip").unwrap();
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_move(x + 100.0, y);
        settle(&mut rt);
        rt.window_focus(false);
        settle(&mut rt);
        assert_eq!(rt.state.files.width, 260.0);
        rt.set_viewport((400.0, 720.0), 1.0);
        settle(&mut rt);
        assert_eq!(rt.state.files.shown_width(400.0), 74.0);
        assert_eq!(rt.state.files.shown_width(200.0), 0.0);
    }
}
