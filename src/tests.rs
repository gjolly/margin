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
