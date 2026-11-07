// SPDX-License-Identifier: GPL-3.0-or-later
//! A Git diff viewer based on the Rune Kit example (MIT, Copyright 2026 Anomaly).
use rune::{App, AppSpec, Cx, El, Key, KeyEvent, Tx, WheelEvent, col, row, text};
use crate::diff::{DiffMode, DiffState, DiffView, parse};
use rune_kit::{Kit, SegmentedControl};

mod diff;
mod search;
mod git;
mod config;
mod code_tools;
mod agent;
mod usage;
mod chat;
mod review_context;
mod scrollbar;
mod file_tree;

// Base size of the whole UI; user zoom is relative to this scale.
const BASE_UI_SCALE: f32 = 1.3;

struct Review {
    diff: DiffState,
    search: search::Search,
    files: file_tree::FilePane,
    message: Option<String>,
    summary: String,
    zoom_percent: u16,
    chats: chat::Chats,
    scrollbar: scrollbar::Scrollbar,
}

impl Review {
    fn ui_scale(&self) -> f32 {
        BASE_UI_SCALE * self.zoom_percent as f32 / 100.0
    }
}

fn init() -> Review {
    let mut diff = DiffState::new(Vec::new());
    diff.set_mode(DiffMode::Split);
    Review { diff, search: search::Search::new(), files: file_tree::FilePane::new(), message: Some("Loading Git diff…".into()), summary: "0 files  +0 −0".into(), zoom_percent: 100, chats: chat::Chats::new(), scrollbar: scrollbar::Scrollbar::new() }
}

fn load_result(s: &mut Review, result: Result<String, String>) {
    match result {
        Ok(patch) => {
            let files = parse(&patch);
            s.message = if files.is_empty() {
                Some(if patch.trim().is_empty() {
                    "No changes for these Git arguments.".into()
                } else {
                    "Git returned output that could not be displayed as a patch.".into()
                })
            } else {
                None
            };
            // The patch is immutable; don't recount its lines during scrolling.
            let (adds, dels) = files.iter().fold((0, 0), |(a, d), f| (a + f.additions(), d + f.deletions()));
            let count = files.len();
            let noun = if count == 1 { "file" } else { "files" };
            s.summary = format!("{count} {noun}  +{adds} −{dels}");
            let files = s.files.load(files);
            s.search.load(search::Snapshot::new(&files));
            s.diff = DiffState::new(files);
            s.diff.set_mode(DiffMode::Split);
        }
        Err(error) => {
            eprintln!("margin: {error}");
            s.message = Some(error);
        }
    }
}

fn view(s: &Review, cx: &mut Cx<Review>) -> El {
    let k = Kit::neutral(cx.system_dark());
    let split = s.diff.mode() == DiffMode::Split;
    let modes = SegmentedControl::new(&["Unified", "Split"], Some(split as usize)).width(Some(160.0)).on_pick(
        |s: &mut Review, i, tx: &mut Tx| {
            tx.motion(rune::tok::motion::SNAPPY);
            s.diff.set_mode(if i == 1 { DiffMode::Split } else { DiffMode::Unified });
        },
    );
    let bar = row()
        .key("bar")
        .w_full()
        .h(48.0)
        .px(16.0)
        .gap(12.0)
        .items_center()
        .fill(k.c.layer_01)
        .child(text("Git diff").font(k.ty.ui_medium).color(k.c.text_base))
        .child(text(s.summary.clone()).font(k.ty.ui).color(k.c.text_muted))
        .child(text(s.chats.notice.clone().unwrap_or_default()).font(k.ty.ui).color(k.c.text_muted).wrap())
        .child(row().grow(1.0))
        .child(file_tree::toggle_button(s, &k))
        .child(modes.build(&k));
    let diff = match &s.message {
        Some(message) => col().w_full().grow(1.0).padding(24.0)
            .child(text(message.clone()).font(k.ty.body).color(k.c.text_muted).wrap()),
        None => {
            let kit = k.clone();
            DiffView::new(|s: &Review| &s.diff, |s: &mut Review| &mut s.diff)
                .key("review-diff")
                .on_wheel(on_zoom_wheel)
                .search(s.search.highlights())
                .on_visible_file(|s, file| s.files.follow(file))
                .on_comment(|s: &mut Review, lines, tx| {
                    tx.instant();
                    s.chats.open(&mut s.diff, lines);
                    s.search.input.focused = false;
                })
                .annotation(move |s, annotation| chat::annotation(s, annotation, &kit))
                .build(s, &k)
        },
    };
    let diff = s.scrollbar.build(diff, &k);
    // Only size changes rebuild this view; scrolling does not.
    let [width, height] = cx.layout_size("viewport").unwrap_or([1100.0, 720.0]);
    let zoom = s.ui_scale();
    let mut content = col()
        .size(width / zoom, height / zoom)
        .absolute(0.0, 0.0)
        .scale(zoom)
        .origin(0.0, 0.0)
        .fill(k.c.bg_base)
        .child(bar)
        .child(row().key("review-body").w_full().grow(1.0).min_h(0.0)
            .child(file_tree::build(s, &k, width / zoom))
            .child(col().key("diff-slot").grow(1.0).min_w(0.0).h_full().child(diff)));
    if s.search.open {
        let available = width / zoom;
        let popup_width = 420.0_f32.min((available - 16.0).max(0.0));
        content = content.child(search::popup(s, &k, available).absolute((available - popup_width - 8.0).max(0.0), 56.0));
    }
    col().key("viewport").w_full().h_full().fill(k.c.bg_base).on_wheel(on_zoom_wheel).child(content)
}

fn on_zoom_wheel(s: &mut Review, ev: WheelEvent, tx: &mut Tx) -> bool {
    if !ev.mods.ctrl || ev.mods.alt || ev.mods.logo || !ev.dy.is_finite() || ev.dy == 0.0 {
        return false;
    }
    s.zoom_percent = if ev.dy > 0.0 {
        s.zoom_percent.saturating_add(10).clamp(50, 200)
    } else {
        s.zoom_percent.saturating_sub(10).clamp(50, 200)
    };
    tx.instant();
    true
}

fn on_key(s: &mut Review, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if let Some(zoom) = zoom_shortcut(s.zoom_percent, ev) {
        s.zoom_percent = zoom;
        tx.instant();
        return true;
    }
    if search::key(s, ev, tx) { return true; }
    if s.search.input.focused || s.chats.threads.iter().any(|t| t.draft.focused) { return false; }
    let handled = s.diff.key(ev, tx);
    if handled { s.files.follow(s.diff.visible_file()); }
    handled
}

fn zoom_shortcut(current: u16, ev: &KeyEvent) -> Option<u16> {
    if !ev.mods.ctrl || ev.mods.alt || ev.mods.logo {
        return None;
    }
    let key = match &ev.key {
        Key::Command(c) => *c,
        Key::Text(text) if text.chars().count() == 1 => text.chars().next()?,
        _ => return None,
    };
    match key {
        '+' | '=' => Some(current.saturating_add(10).clamp(50, 200)),
        '-' => Some(current.saturating_sub(10).clamp(50, 200)),
        '0' => Some(100),
        _ => None,
    }
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        println!("Usage: margin [git diff arguments]\n\nExamples:\n  margin\n  margin --cached\n  margin HEAD~1 HEAD -- src/\n\nRun from the repository you want to inspect. Ctrl++ / Ctrl+- or Ctrl+scroll up/down zoom; Ctrl+0 resets.\nGit output formatting is controlled by the viewer; Rune tooling flags are disabled.");
        return;
    }
    let spec = AppSpec {
        on_key: Some(on_key),
        families: ("Inter", "ui-monospace, Inter"),
        ..AppSpec::new("Margin", (1100.0, 720.0), init, view)
    };
    // All process arguments belong to Git, including its path separator.
    let app = App::new(spec).args(Vec::<String>::new()).modifier_keys();
    let handle = app.handle();
    let mut pending = Some(args);
    app.on_frame_drawn(|rt| {
        let zoom = rt.state.ui_scale();
        rt.state.scrollbar.follow(rt, zoom);
        rt.state.files.scrollbar.follow(rt, zoom);
    })
    .on_ready(move |_| {
        let Some(args) = pending.take() else { return };
        let search_handle = handle.clone();
        handle.update(move |s, tx| {
            s.search.handle = Some(search_handle);
            if s.search.open { search::changed(s, tx); }
        });
        let handle = handle.clone();
        std::thread::spawn(move || {
            let result = git::diff(&args, None);
            let comparison = format!("{args:?}");
            let ai = std::env::current_dir().map_err(|_| "Cannot locate the current code directory.".to_owned())
                .and_then(|cwd| config::load().and_then(|config| agent::Agent::start(config, code_tools::root(&cwd))));
            let chat_handle = handle.clone();
            handle.update(move |s, tx| {
                tx.instant();
                load_result(s, result);
                s.chats.comparison = comparison;
                s.chats.handle = Some(chat_handle);
                match ai {
                    Ok(agent) => { s.chats.agent = Some(agent); s.chats.setup.clear(); }
                    Err(error) => s.chats.setup = error,
                }
                if s.search.open { search::changed(s, tx); }
            });
        });
    }).run();
}

#[cfg(test)]
mod tests;
