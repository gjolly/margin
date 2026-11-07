// SPDX-License-Identifier: GPL-3.0-or-later
//! Session search. A single worker scans immutable patch text; only mounted rows
//! derive highlight ranges, so scrolling never rebuilds a whole-patch match list.
use std::{ops::Range, sync::{Arc, Condvar, Mutex, atomic::{AtomicU64, Ordering}}};

use regex_automata::{meta::Regex, util::syntax};
use rune::{AppHandle, El, Key, KeyEvent, TextEdit, Tx, col, row, text};
use rune_kit::{Button, Kit, TextInput};

use crate::{Review, diff::{FileDiff, LineRef, Side}};

struct SourceLine {
    file: usize,
    old: Option<u32>,
    new: Option<u32>,
    text: String,
}

#[derive(Default)]
pub struct Snapshot(Vec<SourceLine>);

impl Snapshot {
    pub fn new(files: &[FileDiff]) -> Self {
        Self(files.iter().enumerate().flat_map(|(file, f)| f.hunks.iter()
            .flat_map(move |h| h.lines.iter().map(move |l| SourceLine {
                file, old: l.old, new: l.new, text: l.text.clone(),
            }))).collect())
    }
}

struct HitLine {
    source: usize,
    first: usize,
    count: usize,
}

struct Results {
    regex: Regex,
    lines: Vec<HitLine>,
    total: usize,
}

impl Results {
    fn scan(snapshot: &Snapshot, query: &str, cancelled: impl Fn() -> bool) -> Result<Option<Self>, String> {
        let regex = Regex::builder().syntax(syntax::Config::new().case_insensitive(true))
            .build(&regex_syntax::escape(query)).map_err(|_| "Search text is too complex or too long.".to_owned())?;
        let mut result = Self { regex, lines: Vec::new(), total: 0 };
        for (source, line) in snapshot.0.iter().enumerate() {
            if cancelled() { return Ok(None); }
            let count = result.regex.find_iter(line.text.as_str()).count();
            if count != 0 {
                result.lines.push(HitLine { source, first: result.total, count });
                result.total += count;
            }
        }
        Ok(Some(result))
    }

    fn occurrence(&self, snapshot: &Snapshot, index: usize) -> Option<Occurrence> {
        let hit = self.lines.get(self.lines.partition_point(|l| l.first + l.count <= index))?;
        let line = &snapshot.0[hit.source];
        let found = self.regex.find_iter(line.text.as_str()).nth(index - hit.first)?;
        Some(Occurrence { file: line.file, old: line.old, new: line.new, range: found.range() })
    }
}

#[derive(Clone)]
struct Occurrence {
    file: usize,
    old: Option<u32>,
    new: Option<u32>,
    range: Range<usize>,
}

/// The renderer receives only the current matcher and occurrence, not the index.
#[derive(Clone)]
pub struct Highlights {
    pub version: u64,
    regex: Regex,
    active: Option<Occurrence>,
}

impl Highlights {
    pub fn ranges(&self, file: usize, old: Option<u32>, new: Option<u32>, code: &str) -> Vec<(Range<usize>, bool)> {
        let active = self.active.as_ref().filter(|a| a.file == file
            && ((old.is_some() && old == a.old) || (new.is_some() && new == a.new)));
        self.regex.find_iter(code).map(|m| {
            let range = m.range();
            let selected = active.is_some_and(|a| a.range == range);
            (range, selected)
        }).collect()
    }
}

struct Job {
    generation: u64,
    query: String,
    snapshot: Arc<Snapshot>,
}

#[derive(Default)]
struct Mailbox { pending: Option<Job>, stopped: bool }

struct Worker {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    generation: Arc<AtomicU64>,
}

impl Worker {
    fn new(generation: Arc<AtomicU64>, deliver: impl Fn(u64, Result<Option<Results>, String>) + Send + 'static) -> Self {
        let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let work = mailbox.clone();
        let current = generation.clone();
        std::thread::spawn(move || loop {
            let job = {
                let (lock, wake) = &*work;
                let mut slot = lock.lock().unwrap();
                while slot.pending.is_none() && !slot.stopped { slot = wake.wait(slot).unwrap(); }
                if slot.stopped { return; }
                slot.pending.take().unwrap()
            };
            let result = Results::scan(&job.snapshot, &job.query,
                || current.load(Ordering::Relaxed) != job.generation);
            if current.load(Ordering::Relaxed) != job.generation { continue; }
            deliver(job.generation, result);
        });
        Self { mailbox, generation }
    }

    fn submit(&self, job: Job) {
        self.mailbox.0.lock().unwrap().pending = Some(job);
        self.mailbox.1.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        let mut slot = self.mailbox.0.lock().unwrap();
        slot.stopped = true;
        slot.pending = None;
        self.mailbox.1.notify_one();
    }
}

pub struct Search {
    pub open: bool,
    pub input: TextEdit,
    pub handle: Option<AppHandle<Review>>,
    snapshot: Arc<Snapshot>,
    generation: Arc<AtomicU64>,
    worker: Option<Worker>,
    results: Option<Results>,
    active: usize,
    active_match: Option<Occurrence>,
    version: u64,
    pending: bool,
    error: Option<String>,
}

impl Search {
    pub fn new() -> Self {
        Self { open: false, input: TextEdit::new(""), handle: None, snapshot: Arc::default(),
            generation: Arc::new(AtomicU64::new(0)), worker: None, results: None,
            active: 0, active_match: None, version: 0, pending: false, error: None }
    }

    pub fn load(&mut self, snapshot: Snapshot) {
        self.invalidate();
        self.snapshot = Arc::new(snapshot);
    }

    fn invalidate(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.results = None;
        self.pending = false;
        self.error = None;
        self.active = 0;
        self.active_match = None;
        self.version = self.version.wrapping_add(1);
    }

    pub fn highlights(&self) -> Option<Highlights> {
        let results = self.results.as_ref().filter(|_| self.open)?;
        Some(Highlights { version: self.version, regex: results.regex.clone(),
            active: self.active_match.clone() })
    }

    fn status(&self) -> String {
        if let Some(error) = &self.error { return error.clone(); }
        if self.pending { return "Searching…".into(); }
        if self.input.text.is_empty() { return "".into(); }
        match self.results.as_ref().map(|r| r.total).unwrap_or(0) {
            0 => "No matches".into(),
            total => format!("{} / {total}", self.active + 1),
        }
    }
}

pub fn changed(s: &mut Review, tx: &mut Tx) {
    s.search.invalidate();
    tx.instant();
    if !s.search.open || s.search.input.text.is_empty() { return; }
    let Some(handle) = s.search.handle.clone() else { return; };
    s.search.pending = true;
    let worker = s.search.worker.get_or_insert_with(|| Worker::new(s.search.generation.clone(), move |generation, result| {
        handle.update(move |s, tx| accept(s, generation, result, tx));
    }));
    worker.submit(Job { generation: s.search.generation.load(Ordering::Relaxed),
        query: s.search.input.text.clone(), snapshot: s.search.snapshot.clone() });
}

fn accept(s: &mut Review, generation: u64, result: Result<Option<Results>, String>, tx: &mut Tx) {
    if !s.search.open || generation != s.search.generation.load(Ordering::Relaxed) { return; }
    s.search.pending = false;
    match result {
        Ok(results) => s.search.results = results,
        Err(error) => s.search.error = Some(error),
    }
    tx.instant();
    reveal(s, tx);
}

fn reveal(s: &mut Review, tx: &mut Tx) {
    let Some(results) = &s.search.results else { return; };
    let Some(found) = results.occurrence(&s.search.snapshot, s.search.active) else { return; };
    s.search.active_match = Some(found.clone());
    let at = found.new.map(|n| LineRef::new(Side::New, n))
        .unwrap_or_else(|| LineRef::new(Side::Old, found.old.unwrap()));
    s.diff.scroll_to_line(found.file, at, tx);
    s.files.follow(found.file);
}

fn navigate(s: &mut Review, forward: bool, tx: &mut Tx) {
    let Some(total) = s.search.results.as_ref().map(|r| r.total).filter(|n| *n > 0) else { return; };
    s.search.active = if forward { (s.search.active + 1) % total }
        else { s.search.active.checked_sub(1).unwrap_or(total - 1) };
    s.search.version = s.search.version.wrapping_add(1);
    tx.instant();
    reveal(s, tx);
}

pub fn close(s: &mut Review, tx: &mut Tx) {
    s.search.open = false;
    s.search.input.focused = false;
    s.search.invalidate();
    tx.blur();
    tx.instant();
}

pub fn key(s: &mut Review, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let letter = match &ev.key {
        Key::Command(c) => Some(*c),
        Key::Text(t) if t.chars().count() == 1 => t.chars().next(),
        _ => None,
    };
    let find = ev.mods.ctrl && !ev.mods.alt && !ev.mods.logo && letter.is_some_and(|c| c.eq_ignore_ascii_case(&'f'));
    let slash = letter == Some('/') && !ev.mods.ctrl && !ev.mods.alt && !ev.mods.logo
        && !s.search.input.focused && !s.chats.threads.iter().any(|t| t.draft.focused);
    if find || slash {
        if !s.search.open {
            s.search.open = true;
            changed(s, tx);
        }
        for thread in &mut s.chats.threads { thread.draft.focused = false; }
        s.search.input.focused = true;
        s.search.input.set_selection(0, s.search.input.text.len());
        tx.instant();
        return true;
    }
    if s.search.open && ev.key == Key::Escape {
        close(s, tx);
        return true;
    }
    if s.search.open && s.search.input.focused && ev.key == Key::Enter {
        navigate(s, !ev.mods.shift, tx);
        return true;
    }
    false
}

pub fn popup(s: &Review, k: &Kit, available: f32) -> El {
    let disabled = s.search.results.as_ref().is_none_or(|r| r.total == 0);
    col().key("search-popup").w(420.0_f32.min((available - 16.0).max(0.0)))
        .padding(8.0).gap(6.0).radius(8.0).fill(k.c.layer_01)
        .border(1.0, k.c.border_weak).shadow(3.0, 12.0, k.alpha(k.c.text_base, 0.15))
        .child(row().w_full().items_center().gap(4.0)
            .child(TextInput::new(&s.search.input).label("Search diff").placeholder("Find in diff…").width(None)
                .on_edit(|s: &mut Review| &mut s.search.input)
                .on_change(changed).on_key(key).build(k).key("search-field").grow(1.0).min_w(0.0))
            .child(Button::new("↑").disabled(disabled).on_press_tx(|s, tx| navigate(s, false, tx)).build(k).key("search-previous").label("Previous match"))
            .child(Button::new("↓").disabled(disabled).on_press_tx(|s, tx| navigate(s, true, tx)).build(k).key("search-next").label("Next match"))
            .child(Button::new("×").on_press_tx(close).build(k).key("search-close").label("Close search")))
        .child(text(s.search.status()).font(k.ty.ui).color(k.c.text_muted).wrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffMode, parse};
    use rune::{Env, Mods, Runtime};

    const PATCH: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n Foo foo\n-old FOO\n+new foo\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-x\n+foo\n";

    fn state() -> Review {
        let mut s = crate::init();
        crate::load_result(&mut s, Ok(PATCH.into()));
        s
    }

    // Deterministically land the same result as the worker, without a window/event loop.
    fn finish(s: &mut Review, query: &str, tx: &mut Tx) {
        s.search.open = true;
        s.search.input.replace_text(query);
        changed(s, tx);
        let generation = s.search.generation.load(Ordering::Relaxed);
        let result = Results::scan(&s.search.snapshot, query, || false);
        accept(s, generation, result, tx);
    }

    #[test]
    fn matches_literal_unicode_and_context_once_in_patch_order() {
        let s = state();
        let results = Results::scan(&s.search.snapshot, "foo", || false).unwrap().unwrap();
        assert_eq!(results.total, 5);
        assert_eq!(results.occurrence(&s.search.snapshot, 0).unwrap().range, 0..3);
        assert_eq!(results.occurrence(&s.search.snapshot, 1).unwrap().range, 4..7);
        assert_eq!(results.occurrence(&s.search.snapshot, 4).unwrap().file, 1);
        let snapshot = Snapshot(vec![SourceLine { file: 0, old: None, new: Some(1), text: "é É [a.*] é".into() }]);
        let unicode = Results::scan(&snapshot, "É", || false).unwrap().unwrap();
        assert_eq!(unicode.total, 3);
        assert_eq!(unicode.occurrence(&snapshot, 1).unwrap().range, 3..5);
        let literal = Results::scan(&snapshot, "[a.*]", || false).unwrap().unwrap();
        assert_eq!(literal.total, 1);
        assert_eq!(literal.occurrence(&snapshot, 0).unwrap().range, 6..11);
        assert_eq!(Results::scan(&snapshot, "absent", || false).unwrap().unwrap().total, 0);
        assert!(Results::scan(&snapshot, "é", || true).unwrap().is_none());
    }

    #[test]
    fn navigation_wraps_reveals_files_and_survives_mode_changes() {
        let mut s = state();
        let tx = &mut Tx::default();
        s.diff.set_collapsed(1, true);
        finish(&mut s, "foo", tx);
        assert_eq!(s.search.status(), "1 / 5");
        navigate(&mut s, false, tx);
        assert_eq!(s.search.status(), "5 / 5");
        assert_eq!(s.diff.visible_file(), 1);
        assert!(!s.diff.collapsed(1));
        s.diff.set_mode(DiffMode::Unified);
        assert_eq!(s.search.status(), "5 / 5");
        navigate(&mut s, true, tx);
        assert_eq!(s.diff.visible_file(), 0);
        assert_eq!(s.search.active_match.as_ref().unwrap().range, 0..3);
        let h = s.search.highlights().unwrap();
        assert_eq!(h.ranges(0, Some(1), Some(1), "Foo foo"), [(0..3, true), (4..7, false)]);
    }

    #[test]
    fn dismissal_and_new_queries_reject_stale_results() {
        let mut s = state();
        let tx = &mut Tx::default();
        finish(&mut s, "foo", tx);
        let old = s.search.generation.load(Ordering::Relaxed);
        close(&mut s, tx);
        assert!(!s.search.open);
        assert!(s.search.highlights().is_none());
        assert_eq!(s.search.input.text, "foo");
        let stale = Results::scan(&s.search.snapshot, "foo", || false);
        accept(&mut s, old, stale, tx);
        assert!(s.search.results.is_none());
        s.search.open = true;
        s.search.input.replace_text("new");
        changed(&mut s, tx);
        let stale = Results::scan(&s.search.snapshot, "foo", || false);
        accept(&mut s, old, stale, tx);
        assert!(s.search.results.is_none());
        s.search.input.replace_text("");
        changed(&mut s, tx);
        assert!(s.search.highlights().is_none());
        assert_eq!(s.search.status(), "");
    }

    #[test]
    fn worker_scans_off_thread_and_coalesces_pending_queries() {
        use std::{sync::mpsc, time::Duration};
        let current = Arc::new(AtomicU64::new(1));
        let (send, receive) = mpsc::channel();
        let (resume, blocked) = mpsc::channel();
        let worker = Worker::new(current.clone(), move |generation, result| {
            send.send((generation, result.unwrap().unwrap().total)).unwrap();
            if generation == 1 { blocked.recv_timeout(Duration::from_secs(5)).unwrap(); }
        });
        let snapshot = Arc::new(Snapshot::new(&parse(PATCH)));
        worker.submit(Job { generation: 1, query: "foo".into(), snapshot: snapshot.clone() });
        assert_eq!(receive.recv_timeout(Duration::from_secs(5)).unwrap(), (1, 5));
        // Hold the worker between scans while successive edits replace the pending job.
        current.store(2, Ordering::Relaxed);
        worker.submit(Job { generation: 2, query: "old".into(), snapshot: snapshot.clone() });
        current.store(3, Ordering::Relaxed);
        worker.submit(Job { generation: 3, query: "new".into(), snapshot });
        resume.send(()).unwrap();
        assert_eq!(receive.recv_timeout(Duration::from_secs(5)).unwrap(), (3, 1));
        assert!(receive.try_recv().is_err());
    }

    fn settle(rt: &mut Runtime<Review>) {
        for _ in 0..45 { rt.frame(1.0 / 60.0); }
    }

    fn press(rt: &mut Runtime<Review>, key: Key, mods: Mods) {
        rt.key(KeyEvent { key, mods });
        settle(rt);
    }

    fn ctrl_f(rt: &mut Runtime<Review>) {
        press(rt, Key::Command('f'), Mods { ctrl: true, cmd: true, ..Mods::default() });
    }

    fn click(rt: &mut Runtime<Review>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        settle(rt);
    }

    #[test]
    fn popup_focus_typing_navigation_and_both_dismissal_paths() {
        let mut rt = Runtime::new(state(), crate::view, (1100.0, 720.0), Env::new(true, 1.0));
        rt.on_key = Some(crate::on_key);
        settle(&mut rt);
        press(&mut rt, Key::Text("/".into()), Mods::default());
        assert!(rt.state.search.open);
        assert!(rt.state.search.input.focused);
        assert_eq!(rt.state.search.input.text, "");
        for key in ["j", "k", "[", "]", "/"] { press(&mut rt, Key::Text(key.into()), Mods::default()); }
        assert_eq!(rt.state.search.input.text, "jk[]/");
        ctrl_f(&mut rt);
        press(&mut rt, Key::Text("foo".into()), Mods::default());
        assert_eq!(rt.state.search.input.text, "foo");
        rt.update(|s, tx| finish(s, "foo", tx));
        settle(&mut rt);
        press(&mut rt, Key::Enter, Mods::default());
        assert_eq!(rt.state.search.active, 1);
        press(&mut rt, Key::Enter, Mods { shift: true, ..Mods::default() });
        assert_eq!(rt.state.search.active, 0);
        click(&mut rt, "search-next");
        assert_eq!(rt.state.search.active, 1);
        click(&mut rt, "search-previous");
        assert_eq!(rt.state.search.active, 0);
        click(&mut rt, "search-close");
        assert!(!rt.state.search.open);
        assert!(rt.state.search.highlights().is_none());
        assert!(rt.find("search-popup").is_none());
        ctrl_f(&mut rt);
        assert_eq!(rt.state.search.input.text, "foo");
        rt.update(|s, tx| finish(s, "foo", tx));
        settle(&mut rt);
        press(&mut rt, Key::Escape, Mods::default());
        assert!(!rt.state.search.open);
        assert!(rt.state.search.highlights().is_none());
    }

    #[test]
    fn popup_is_within_viewport_at_all_zoom_levels_and_narrow_sizes() {
        for zoom in [50, 100, 200] {
            for width in [400.0, 1100.0] {
                let mut s = state();
                s.zoom_percent = zoom;
                s.search.open = true;
                let mut rt = Runtime::new(s, crate::view, (width, 720.0), Env::new(true, 1.0));
                settle(&mut rt);
                let (x, y) = rt.find("search-close").expect("close button remains accessible");
                assert!(x >= 0.0 && x < width && y > 0.0 && y < 720.0, "{x}, {y} at width {width}, zoom {zoom}");
            }
        }
    }

    #[test]
    fn slash_is_chat_text_and_ctrl_f_moves_focus_to_search() {
        let mut s = state();
        s.chats.open(&mut s.diff, crate::diff::LineRange::line(0, LineRef::new(Side::New, 1)));
        s.chats.threads[0].draft.focused = true;
        let mut rt = Runtime::new(s, crate::view, (1100.0, 720.0), Env::new(true, 1.0));
        rt.on_key = Some(crate::on_key);
        settle(&mut rt);
        press(&mut rt, Key::Text("/".into()), Mods::default());
        assert_eq!(rt.state.chats.threads[0].draft.text, "/");
        assert!(!rt.state.search.open);
        ctrl_f(&mut rt);
        assert!(rt.state.search.open);
        assert!(rt.state.search.input.focused);
        assert!(!rt.state.chats.threads[0].draft.focused);
        press(&mut rt, Key::Text("foo".into()), Mods::default());
        assert_eq!(rt.state.search.input.text, "foo");
        assert_eq!(rt.state.chats.threads[0].draft.text, "/");
    }

    #[test]
    fn large_results_keep_rows_virtual_and_settle_without_polling() {
        let mut s = crate::init();
        let mut patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,10000 +1,10000 @@\n".to_owned();
        for _ in 0..10000 { patch.push_str("-old foo foo\n+new foo foo\n"); }
        crate::load_result(&mut s, Ok(patch));
        finish(&mut s, "foo", &mut Tx::default());
        let mut rt = Runtime::new(s, crate::view, (1100.0, 720.0), Env::new(true, 1.0));
        settle(&mut rt);
        assert_eq!(rt.state.search.results.as_ref().unwrap().total, 40000);
        assert_eq!(rt.state.search.results.as_ref().unwrap().lines.len(), 20000);
        let snapshot = rt.state.search.snapshot.clone();
        rt.wheel(900.0, 600.0, -2000.0);
        settle(&mut rt);
        assert!(Arc::ptr_eq(&snapshot, &rt.state.search.snapshot));
        let mounted = rt.elements().iter().filter(|e| e.text.as_deref() == Some("new foo foo")).count();
        assert!(mounted < 200, "mounted {mounted} lines");
        for _ in 0..5 { assert!(!rt.frame(1.0 / 60.0), "search requested an idle frame"); }
    }

    #[test]
    fn empty_and_binary_patches_have_no_matches() {
        for patch in ["", "diff --git a/a b/a\nBinary files a/a and b/a differ\n"] {
            let snapshot = Snapshot::new(&parse(patch));
            assert_eq!(Results::scan(&snapshot, "a", || false).unwrap().unwrap().total, 0);
        }
    }
}
