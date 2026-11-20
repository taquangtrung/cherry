//! Conversions between misty's keys and `cherry-keys`' keys, for an app
//! that resolves its keymap with Cherry. Both normalize a chord the same
//! way, so a key converts both ways without loss.

use misty::{Key, KeyCode, Mods};

// ========================================================================
// Constants
// ========================================================================

/// Each misty modifier beside the Cherry modifier it is.
const MODS: [(Mods, cherry_engine::keys::Mods); 4] = [
    (Mods::ALT, cherry_engine::keys::Mods::ALT),
    (Mods::CTRL, cherry_engine::keys::Mods::CTRL),
    (Mods::SHIFT, cherry_engine::keys::Mods::SHIFT),
    (Mods::SUPER, cherry_engine::keys::Mods::SUPER),
];

// ========================================================================
// Functions
// ========================================================================

/// `key`, a misty key, as the Cherry key it is; `None` for a key Cherry has
/// no code for, which an editor ignores.
pub fn to_cherry(key: Key) -> Option<cherry_engine::keys::Key> {
    let code = code_to_cherry(key.code)?;
    Some(cherry_engine::keys::Key::new(
        code,
        mods_to_cherry(key.mods),
    ))
}

/// `key`, a Cherry key, as the misty key it is.
pub fn from_cherry(key: cherry_engine::keys::Key) -> Key {
    Key::new(code_from_cherry(key.code), mods_from_cherry(key.mods))
}

/// `mods`, misty modifiers, as the Cherry modifiers they are.
pub fn mods_to_cherry(mods: Mods) -> cherry_engine::keys::Mods {
    MODS.iter()
        .filter(|(ours, _)| mods.contains(*ours))
        .fold(cherry_engine::keys::Mods::NONE, |held, (_, theirs)| {
            held | *theirs
        })
}

/// `mods`, Cherry modifiers, as the misty modifiers they are.
fn mods_from_cherry(mods: cherry_engine::keys::Mods) -> Mods {
    MODS.iter()
        .filter(|(_, theirs)| mods.contains(*theirs))
        .fold(Mods::NONE, |held, (ours, _)| held | *ours)
}

/// `code`, a misty key code, as the Cherry key code it is, if Cherry has one.
fn code_to_cherry(code: KeyCode) -> Option<cherry_engine::keys::KeyCode> {
    let code = match code {
        KeyCode::Backspace => cherry_engine::keys::KeyCode::Backspace,
        KeyCode::Char(c) => cherry_engine::keys::KeyCode::Char(c),
        KeyCode::Delete => cherry_engine::keys::KeyCode::Delete,
        KeyCode::Down => cherry_engine::keys::KeyCode::Down,
        KeyCode::End => cherry_engine::keys::KeyCode::End,
        KeyCode::Enter => cherry_engine::keys::KeyCode::Enter,
        KeyCode::Esc => cherry_engine::keys::KeyCode::Esc,
        KeyCode::F(n) => cherry_engine::keys::KeyCode::F(n),
        KeyCode::Home => cherry_engine::keys::KeyCode::Home,
        KeyCode::Insert => cherry_engine::keys::KeyCode::Insert,
        KeyCode::Left => cherry_engine::keys::KeyCode::Left,
        KeyCode::PageDown => cherry_engine::keys::KeyCode::PageDown,
        KeyCode::PageUp => cherry_engine::keys::KeyCode::PageUp,
        KeyCode::Right => cherry_engine::keys::KeyCode::Right,
        KeyCode::Tab => cherry_engine::keys::KeyCode::Tab,
        KeyCode::Up => cherry_engine::keys::KeyCode::Up,
        // A key code misty adds later has no Cherry code yet.
        _ => return None,
    };
    Some(code)
}

/// `code`, a Cherry key code, as the misty key code it is.
fn code_from_cherry(code: cherry_engine::keys::KeyCode) -> KeyCode {
    match code {
        cherry_engine::keys::KeyCode::Backspace => KeyCode::Backspace,
        cherry_engine::keys::KeyCode::Char(c) => KeyCode::Char(c),
        cherry_engine::keys::KeyCode::Delete => KeyCode::Delete,
        cherry_engine::keys::KeyCode::Down => KeyCode::Down,
        cherry_engine::keys::KeyCode::End => KeyCode::End,
        cherry_engine::keys::KeyCode::Enter => KeyCode::Enter,
        cherry_engine::keys::KeyCode::Esc => KeyCode::Esc,
        cherry_engine::keys::KeyCode::F(n) => KeyCode::F(n),
        cherry_engine::keys::KeyCode::Home => KeyCode::Home,
        cherry_engine::keys::KeyCode::Insert => KeyCode::Insert,
        cherry_engine::keys::KeyCode::Left => KeyCode::Left,
        cherry_engine::keys::KeyCode::PageDown => KeyCode::PageDown,
        cherry_engine::keys::KeyCode::PageUp => KeyCode::PageUp,
        cherry_engine::keys::KeyCode::Right => KeyCode::Right,
        cherry_engine::keys::KeyCode::Tab => KeyCode::Tab,
        cherry_engine::keys::KeyCode::Up => KeyCode::Up,
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use misty::parse_key_sequence;

    use super::*;

    #[test]
    fn test_keys_convert_both_ways_without_loss() {
        let keys = parse_key_sequence("a G ? C-S-f M-? S-<tab> C-M-S-s-<f24> <esc> <space>")
            .expect("valid keys");
        for key in keys {
            let theirs = to_cherry(key).expect("a Cherry key");
            assert_eq!(theirs.to_string(), key.to_string());
            assert_eq!(from_cherry(theirs), key);
        }
    }
}
