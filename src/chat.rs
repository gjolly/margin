// SPDX-License-Identifier: GPL-3.0-or-later
//! Session-owned conversations and inline presentation; workers own all I/O.
use crate::{Review, agent::{self, Agent, Answer}, review_context};
use rune::{AppHandle, El, Key, KeyEvent, TextEdit, Tx, col, row, text};
use rune_kit::{Button, Kit, Markdown, Spinner, TextArea};
use rune_kit::diff::{Annotation, DiffState, LineRange, Side};
use serde_json::{Value, json};

const THREAD_LIMIT: usize = 64;
const ACTIVE_LIMIT: usize = 4;

pub struct Post {
    pub user: bool,
    pub body: String,
    pub stats: Option<String>,
}

pub struct Thread {
    pub anchor: LineRange,
    pub draft: TextEdit,
    pub history: Vec<Value>,
    pub posts: Vec<Post>,
    pub busy: bool,
    pub activity: String,
    pub pending: bool,
    pub error: Option<String>,
}

pub struct Chats {
    pub threads: Vec<Thread>,
    pub agent: Option<Agent>,
    pub setup: String,
    pub comparison: String,
    pub handle: Option<AppHandle<Review>>,
    pub notice: Option<String>,
}

impl Chats {
    pub fn new() -> Self {
        Self { threads: Vec::new(), agent: None, setup: "Loading AI configuration…".into(),
            comparison: String::new(), handle: None, notice: None }
    }

    pub fn open(&mut self, diff: &mut DiffState, mut anchor: LineRange) {
        let file = &diff.files()[anchor.file];
        let position = |at: rune_kit::diff::LineRef| file.hunks.iter().flat_map(|h| &h.lines)
            .position(|line| line.number(at.side) == Some(at.line));
        if matches!((position(anchor.start), position(anchor.end)), (Some(a), Some(b)) if a > b) {
            std::mem::swap(&mut anchor.start, &mut anchor.end);
        }
        if self.threads.iter().any(|t| t.anchor == anchor) { return; }
        if self.threads.len() >= THREAD_LIMIT {
            self.notice = Some("Session reached its 64-thread limit. Restart to begin a new session.".into());
            return;
        }
        let context = review_context::build(diff, anchor, &self.comparison);
        self.threads.push(Thread { anchor, draft: TextEdit::new(""), posts: Vec::new(), busy: false,
            activity: String::new(), pending: false, error: None, history: vec![
                json!({"role":"system","content":review_context::SYSTEM_PROMPT}),
                json!({"role":"user","content":context}),
            ] });
        diff.set_annotations(self.threads.iter().enumerate().map(|(id, t)|
            Annotation::new(t.anchor.file, t.anchor.end, id.to_string())).collect());
    }

    pub fn active(&self) -> usize { self.threads.iter().filter(|t| t.busy).count() }

    pub fn prepare(&mut self, id: usize, retry: bool) -> Option<Vec<Value>> {
        if self.active() >= ACTIVE_LIMIT { return None; }
        let thread = self.threads.get_mut(id)?;
        if thread.busy || retry != thread.pending { return None; }
        if !retry {
            let content = thread.draft.text.trim().to_owned();
            if content.is_empty() { return None; }
            let message = json!({"role":"user","content":content});
            let bytes = serde_json::to_vec(&thread.history).unwrap().len() + message.to_string().len() + 1;
            if bytes > agent::HISTORY_LIMIT {
                thread.error = Some("Conversation reached its 512 KiB history limit. Shorten the draft or start a thread on another line.".into());
                return None;
            }
            thread.history.push(message);
            thread.posts.push(Post { user: true, body: content, stats: None });
            thread.draft.clear();
            thread.pending = true;
        }
        thread.error = None;
        thread.busy = true;
        thread.activity = "Waiting for AI response…".into();
        Some(thread.history.clone())
    }

    pub fn progress(&mut self, id: usize, activity: String) {
        if let Some(thread) = self.threads.get_mut(id).filter(|t| t.busy) {
            thread.activity = activity;
        }
    }

    pub fn finish(&mut self, id: usize, answer: Result<Answer, String>) {
        let Some(thread) = self.threads.get_mut(id) else { return };
        thread.busy = false;
        thread.activity.clear();
        match answer {
            Ok(answer) => {
                thread.history = answer.history;
                thread.posts.push(Post { user: false, body: answer.text, stats: Some(answer.stats.label()) });
                thread.pending = false;
                thread.error = None;
            }
            Err(error) => thread.error = Some(error),
        }
    }
}

pub fn send(s: &mut Review, id: usize, retry: bool, tx: &mut Tx) {
    let (Some(agent), Some(handle)) = (s.chats.agent.clone(), s.chats.handle.clone()) else { return };
    let Some(history) = s.chats.prepare(id, retry) else { return };
    tx.instant();
    let progress_handle = handle.clone();
    if !agent.send(history, move |activity| {
        progress_handle.update(move |s, tx| { tx.instant(); s.chats.progress(id, activity); });
    }, move |answer| {
        handle.update(move |s, tx| { tx.instant(); s.chats.finish(id, answer); });
    }) {
        s.chats.finish(id, Err("AI worker is unavailable. Restart margin.".into()));
    }
}

pub fn annotation(s: &Review, a: &Annotation, k: &Kit) -> El {
    let id: usize = a.id.parse().unwrap();
    let t = &s.chats.threads[id];
    let side = |side| if side == Side::Old { "old" } else { "new" };
    let mut card = col().w_full().padding(12.0).gap(8.0).radius(8.0)
        .fill(k.c.layer_01).border(1.0, k.c.border_weak)
        .child(text(format!("AI chat · {} {} – {} {}", side(t.anchor.start.side), t.anchor.start.line, side(t.anchor.end.side), t.anchor.end.line))
            .font(k.ty.ui_medium).color(k.c.text_muted));
    for (i, message) in t.posts.iter().enumerate() {
        let Post { user, body, stats } = message;
        let mut post = col().key(format!("post-{i}")).w_full().gap(4.0)
            .child(text(if *user { "You" } else { "Assistant" }).font(k.ty.ui_medium).color(k.c.text_base));
        post = if *user { post.child(text(body.clone()).font(k.ty.body).color(k.c.text_base).wrap()) }
            else { post.child(Markdown::new(body.clone()).tasks_read_only().selectable(true).scroll_wide(true).build(k)) };
        if let Some(stats) = stats {
            post = post.child(text(stats.clone()).font(k.ty.ui).color(k.c.text_faint).wrap());
        }
        card = card.child(post);
    }
    if t.busy {
        card = card.child(row().gap(6.0).items_center().child(Spinner::new().size(14.0).build(k))
            .child(text(t.activity.clone()).font(k.ty.ui).color(k.c.text_muted).wrap()));
    }
    if let Some(error) = &t.error {
        card = card.child(text(error.clone()).font(k.ty.ui).color(k.c.text_muted).wrap());
        if t.pending {
            card = card.child(Button::new("Retry").disabled(t.busy || s.chats.active() >= ACTIVE_LIMIT)
                .on_press_tx(move |s: &mut Review, tx| send(s, id, true, tx)).build(k));
        }
    }
    if s.chats.agent.is_none() {
        card = card.child(text(s.chats.setup.clone()).font(k.ty.ui).color(k.c.text_muted).wrap());
    }
    card = card.child(composer(s, id, k));
    let disabled = s.chats.agent.is_none() || t.busy || t.pending || t.draft.text.trim().is_empty() || s.chats.active() >= ACTIVE_LIMIT;
    card = card.child(row().gap(8.0).items_center()
        .child(Button::new("Send").disabled(disabled).on_press_tx(move |s: &mut Review, tx| send(s, id, false, tx)).build(k))
        .child(text(if s.chats.active() >= ACTIVE_LIMIT { "Four conversations are working; wait to send." } else { "Ctrl+Enter to send · Session only" })
            .font(k.ty.ui).color(k.c.text_faint).wrap()));
    col().key(format!("chat-{id}")).w_full().px(12.0).py(8.0).fill(k.c.bg_base).child(card)
}

fn composer(s: &Review, id: usize, k: &Kit) -> El {
    TextArea::new(&s.chats.threads[id].draft).label("Message to AI").placeholder("Ask about this code…")
        .width(None).auto_grow(2, 6)
        .on_edit(move |s: &mut Review| &mut s.chats.threads[id].draft)
        .on_key(move |s: &mut Review, event: &KeyEvent, tx| {
            if event.key != Key::Enter { return false; }
            if event.mods.ctrl && !event.mods.alt && !event.mods.logo {
                if !tx.key_repeat() { send(s, id, false, tx); }
            } else if !event.mods.ctrl && !event.mods.alt && !event.mods.logo {
                s.chats.threads[id].draft.insert("\n");
                tx.instant();
            }
            true
        }).build(k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune_kit::diff::{LineRef, parse};

    fn fixture() -> (Chats, DiffState, LineRange) {
        let diff = DiffState::new(parse("diff --git a/old.rs b/new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-deleted historical code\n+new code\n"));
        (Chats::new(), diff, LineRange::line(0, LineRef::new(Side::Old, 1)))
    }

    #[test]
    fn anchors_and_drafts_survive_reopening_and_retry() {
        let (mut chats, mut diff, anchor) = fixture();
        chats.open(&mut diff, anchor);
        chats.threads[0].draft.replace_text("Explain this");
        chats.open(&mut diff, anchor);
        assert_eq!(chats.threads.len(), 1);
        assert_eq!(chats.threads[0].draft.text, "Explain this");
        assert!(chats.threads[0].history[1]["content"].as_str().unwrap().contains("deleted historical code"));
        let history = chats.prepare(0, false).unwrap();
        assert!(chats.prepare(0, false).is_none());
        chats.finish(0, Err("network error".into()));
        let retry = chats.prepare(0, true).unwrap();
        assert_eq!(retry, history);
        assert_eq!(chats.threads[0].posts.len(), 1);
        chats.finish(0, Ok(Answer { history, text: "Answer".into(), stats: Default::default() }));
        assert_eq!(chats.threads[0].posts.len(), 2);
        assert!(chats.threads[0].posts[0].stats.is_none());
        assert!(chats.threads[0].posts[1].stats.as_ref().unwrap().contains("Token usage not reported"));
        assert!(!chats.threads[0].pending);
        assert!(!chats.threads[0].busy);
    }

    #[test]
    fn activity_is_isolated_cleared_and_reset_on_retry() {
        let (mut chats, mut diff, anchor) = fixture();
        chats.open(&mut diff, anchor);
        chats.open(&mut diff, LineRange::line(0, LineRef::new(Side::New, 1)));
        for id in 0..2 {
            chats.threads[id].draft.replace_text("question");
            chats.prepare(id, false).unwrap();
        }
        chats.progress(0, "Reading sample.rs…".into());
        assert_eq!(chats.threads[0].activity, "Reading sample.rs…");
        assert_eq!(chats.threads[1].activity, "Waiting for AI response…");
        chats.finish(0, Err("failed".into()));
        assert!(chats.threads[0].activity.is_empty());
        chats.progress(0, "late update".into());
        assert!(chats.threads[0].activity.is_empty());
        chats.prepare(0, true).unwrap();
        assert_eq!(chats.threads[0].activity, "Waiting for AI response…");
        chats.finish(0, Ok(Answer { history: Vec::new(), text: "Done".into(), stats: Default::default() }));
        assert!(chats.threads[0].activity.is_empty());
        assert!(!chats.threads[0].busy);
    }

    #[test]
    fn empty_drafts_limits_and_pending_turns_are_enforced() {
        let (mut chats, mut diff, anchor) = fixture();
        for n in 1..=65 {
            chats.open(&mut diff, LineRange::line(0, rune_kit::diff::LineRef::new(anchor.start.side, n)));
        }
        assert_eq!(chats.threads.len(), 64);
        assert!(chats.notice.is_some());
        assert!(chats.prepare(0, false).is_none());
        for id in 0..4 {
            chats.threads[id].draft.replace_text("question");
            assert!(chats.prepare(id, false).is_some());
        }
        chats.threads[4].draft.replace_text("question");
        assert!(chats.prepare(4, false).is_none());
        chats.finish(0, Err("failed".into()));
        assert!(chats.prepare(0, false).is_none());
        chats.threads[4].draft.replace_text("x".repeat(agent::HISTORY_LIMIT));
        assert!(chats.prepare(4, false).is_none());
        assert!(!chats.threads[4].draft.text.is_empty());
    }

    #[test]
    fn reversed_and_cross_side_selections_include_exact_patch_code() {
        let (mut chats, mut diff, anchor) = fixture();
        let end = LineRef::new(Side::New, 1);
        chats.open(&mut diff, LineRange { file: 0, start: end, end: anchor.start });
        let canonical = LineRange { file: 0, start: anchor.start, end };
        assert_eq!(chats.threads[0].anchor, canonical);
        chats.open(&mut diff, canonical);
        assert_eq!(chats.threads.len(), 1);
        let text = chats.threads[0].history[1]["content"].as_str().unwrap();
        assert!(text.contains("deleted historical code"));
        assert!(text.contains("new code"));
        assert_eq!(diff.annotations()[0].at, end);
        diff.set_mode(rune_kit::diff::DiffMode::Unified);
        diff.set_collapsed(0, true);
        diff.set_collapsed(0, false);
        assert_eq!(diff.annotations().len(), 1);
    }

    #[test]
    fn composer_routes_typing_newlines_and_ctrl_enter_without_a_renderer() {
        fn input(s: &Review, _: &mut rune::Cx<Review>) -> El {
            composer(s, 0, &Kit::neutral(true))
        }
        let mut state = crate::init();
        let (chats, diff, anchor) = fixture();
        state.chats = chats;
        state.diff = diff;
        state.chats.open(&mut state.diff, anchor);
        state.chats.threads[0].draft.focused = true;
        let mut runtime = rune::Runtime::new(state, input, (600.0, 400.0), rune::Env::new(true, 1.0));
        runtime.on_key = Some(crate::on_key);
        runtime.frame(0.0);
        for key in ["j", "k", "[", "]"] {
            runtime.key(KeyEvent::from(Key::Text(key.into())));
            runtime.frame(0.0);
        }
        assert_eq!(runtime.state.chats.threads[0].draft.text, "jk[]");
        runtime.key(KeyEvent::from(Key::Enter));
        runtime.frame(0.0);
        assert_eq!(runtime.state.chats.threads[0].draft.text, "jk[]\n");
        runtime.key(KeyEvent { key: Key::Enter, mods: rune::Mods { ctrl: true, cmd: true, ..Default::default() } });
        runtime.frame(0.0);
        // No provider is configured: Ctrl+Enter preserves the draft rather than adding a newline.
        assert_eq!(runtime.state.chats.threads[0].draft.text, "jk[]\n");
        assert!(runtime.state.chats.threads[0].posts.is_empty());
    }
}
