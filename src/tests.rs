// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use rune::Mods;

fn ctrl(key: Key) -> KeyEvent {
    KeyEvent { key, mods: Mods { ctrl: true, cmd: true, ..Mods::default() } }
}

#[test]
fn zoom_keys_handle_command_and_text_events() {
    for key in [Key::Command('+'), Key::Command('='), Key::Text("+".into())] {
        assert_eq!(zoom_shortcut(100, &ctrl(key)), Some(110));
    }
    assert_eq!(zoom_shortcut(100, &ctrl(Key::Command('-'))), Some(90));
    assert_eq!(zoom_shortcut(170, &ctrl(Key::Command('0'))), Some(100));
}

#[test]
fn zoom_is_bounded_and_reversible() {
    assert_eq!(zoom_shortcut(200, &ctrl(Key::Command('+'))), Some(200));
    assert_eq!(zoom_shortcut(50, &ctrl(Key::Command('-'))), Some(50));
    let bigger = zoom_shortcut(100, &ctrl(Key::Command('+'))).unwrap();
    assert_eq!(zoom_shortcut(bigger, &ctrl(Key::Command('-'))), Some(100));
}

#[test]
fn plain_text_and_altgr_are_not_zoom_shortcuts() {
    assert_eq!(zoom_shortcut(100, &KeyEvent::from(Key::Text("+".into()))), None);
    let mut event = ctrl(Key::Text("+".into()));
    event.mods.alt = true;
    assert_eq!(zoom_shortcut(100, &event), None);
    assert_eq!(zoom_shortcut(100, &ctrl(Key::Command('c'))), None);
}

#[test]
fn git_results_replace_loading_with_split_view_or_a_message() {
    let mut state = init();
    load_result(&mut state, Ok("diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n".into()));
    assert_eq!(state.diff.files().len(), 1);
    assert_eq!(state.summary, "1 file  +1 −1");
    assert_eq!(state.diff.mode(), DiffMode::Split);
    assert!(state.message.is_none());
    load_result(&mut state, Ok(String::new()));
    assert!(state.diff.files().is_empty());
    assert_eq!(state.summary, "0 files  +0 −0");
    assert!(state.message.as_ref().unwrap().contains("No changes"));
    load_result(&mut state, Err("Invalid revision".into()));
    assert_eq!(state.message.as_deref(), Some("Invalid revision"));
}

#[test]
fn missing_ai_setup_keeps_drafts_and_zoom_still_works() {
    let mut state = init();
    load_result(&mut state, Ok("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n".into()));
    let anchor = rune_kit::diff::LineRange::line(0, rune_kit::diff::LineRef::new(rune_kit::diff::Side::New, 1));
    state.chats.open(&mut state.diff, anchor);
    state.chats.threads[0].draft.replace_text("j k [ ]");
    state.chats.threads[0].draft.focused = true;
    chat::send(&mut state, 0, false, &mut Tx::default());
    assert_eq!(state.chats.threads[0].draft.text, "j k [ ]");
    assert!(state.chats.threads[0].posts.is_empty());
    assert!(on_key(&mut state, &ctrl(Key::Command('+')), &mut Tx::default()));
    assert_eq!(state.zoom_percent, 110);
}
