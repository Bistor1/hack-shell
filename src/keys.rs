use alacritty_terminal::term::TermMode;
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub fn encode(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    let bytes = match &event.logical_key {
        Key::Named(NamedKey::Enter) => b"\r".to_vec(),
        Key::Named(NamedKey::Backspace) => {
            if ctrl { vec![0x08] } else { vec![0x7f] }
        },
        Key::Named(NamedKey::Tab) => {
            if shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }
        },
        Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
        Key::Named(NamedKey::ArrowUp) => arrow(b'A', app_cursor, mods),
        Key::Named(NamedKey::ArrowDown) => arrow(b'B', app_cursor, mods),
        Key::Named(NamedKey::ArrowRight) => arrow(b'C', app_cursor, mods),
        Key::Named(NamedKey::ArrowLeft) => arrow(b'D', app_cursor, mods),
        Key::Named(NamedKey::Home) => csi_or_ss3(b'H', app_cursor, mods),
        Key::Named(NamedKey::End) => csi_or_ss3(b'F', app_cursor, mods),
        Key::Named(NamedKey::Delete) => modified(b"\x1b[3~", 3, mods),
        Key::Named(NamedKey::Insert) => modified(b"\x1b[2~", 2, mods),
        Key::Named(NamedKey::PageUp) => modified(b"\x1b[5~", 5, mods),
        Key::Named(NamedKey::PageDown) => modified(b"\x1b[6~", 6, mods),
        Key::Named(NamedKey::F1) => b"\x1bOP".to_vec(),
        Key::Named(NamedKey::F2) => b"\x1bOQ".to_vec(),
        Key::Named(NamedKey::F3) => b"\x1bOR".to_vec(),
        Key::Named(NamedKey::F4) => b"\x1bOS".to_vec(),
        Key::Named(NamedKey::F5) => b"\x1b[15~".to_vec(),
        Key::Named(NamedKey::F6) => b"\x1b[17~".to_vec(),
        Key::Named(NamedKey::F7) => b"\x1b[18~".to_vec(),
        Key::Named(NamedKey::F8) => b"\x1b[19~".to_vec(),
        Key::Named(NamedKey::F9) => b"\x1b[20~".to_vec(),
        Key::Named(NamedKey::F10) => b"\x1b[21~".to_vec(),
        Key::Named(NamedKey::F11) => b"\x1b[23~".to_vec(),
        Key::Named(NamedKey::F12) => b"\x1b[24~".to_vec(),
        Key::Character(s) => {
            let ch = s.chars().next()?;
            if ctrl && ch.is_ascii_alphabetic() && !alt {
                return Some(vec![ch.to_ascii_lowercase() as u8 - b'a' + 1]);
            }
            if ctrl && ch == ' ' {
                return Some(vec![0x00]);
            }
            if alt {
                let mut v = vec![0x1b];
                v.extend(s.as_bytes());
                return Some(v);
            }
            s.as_bytes().to_vec()
        },
        _ => return None,
    };
    Some(bytes)
}

fn arrow(letter: u8, app_cursor: bool, mods: ModifiersState) -> Vec<u8> {
    let m = mod_param(mods);
    if m == 1 && app_cursor {
        vec![0x1b, b'O', letter]
    } else if m == 1 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{m}{}", letter as char).into_bytes()
    }
}

fn csi_or_ss3(letter: u8, app_cursor: bool, mods: ModifiersState) -> Vec<u8> {
    let m = mod_param(mods);
    if m == 1 && app_cursor {
        vec![0x1b, b'O', letter]
    } else if m == 1 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{m}{}", letter as char).into_bytes()
    }
}

fn modified(plain: &[u8], code: u8, mods: ModifiersState) -> Vec<u8> {
    let m = mod_param(mods);
    if m == 1 {
        plain.to_vec()
    } else {
        format!("\x1b[{code};{m}~").into_bytes()
    }
}

fn mod_param(mods: ModifiersState) -> u8 {
    let mut m = 1u8;
    if mods.shift_key() {
        m += 1;
    }
    if mods.alt_key() {
        m += 2;
    }
    if mods.control_key() {
        m += 4;
    }
    m
}

pub fn bracket_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut v = b"\x1b[200~".to_vec();
        v.extend(text.as_bytes());
        v.extend(b"\x1b[201~");
        v
    } else {
        text.replace('\r', "").replace('\n', "\r").into_bytes()
    }
}

pub fn mouse_report(button: u8, col: usize, row: usize, release: bool, mods: ModifiersState) -> Vec<u8> {
    let mut b = button;
    if mods.shift_key() {
        b += 4;
    }
    if mods.alt_key() {
        b += 8;
    }
    if mods.control_key() {
        b += 16;
    }
    let end = if release { 'm' } else { 'M' };
    format!("\x1b[<{b};{};{row}{end}", col.max(1)).into_bytes()
}
