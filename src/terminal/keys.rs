//! Xterm-style keyboard encoding. Plain text is committed by GPUI's input handler.
use alacritty_terminal::term::TermMode;
use gpui_kit::Keystroke;

pub(super) fn encode(key: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    let parameter = 1
        + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt)
        + 4 * u8::from(modifiers.control);
    let arrow = match key.key.as_str() {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(final_byte) = arrow {
        return Some(
            if parameter > 1 {
                format!("\x1b[1;{parameter}{final_byte}")
            } else if mode.contains(TermMode::APP_CURSOR) {
                format!("\x1bO{final_byte}")
            } else {
                format!("\x1b[{final_byte}")
            }
            .into_bytes(),
        );
    }
    let numbered = match key.key.as_str() {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(n) = numbered {
        return Some(
            if parameter > 1 {
                format!("\x1b[{n};{parameter}~")
            } else {
                format!("\x1b[{n}~")
            }
            .into_bytes(),
        );
    }
    if let Some(final_byte) = match key.key.as_str() {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    } {
        return Some(
            if parameter > 1 {
                format!("\x1b[1;{parameter}{final_byte}")
            } else {
                format!("\x1bO{final_byte}")
            }
            .into_bytes(),
        );
    }
    let value = match key.key.as_str() {
        "enter" => Some(vec![b'\r']),
        "tab" if modifiers.shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(vec![b'\t']),
        "backspace" => Some(vec![if modifiers.control { 8 } else { 127 }]),
        // Escape is reserved for Rockstar, never emitted here.
        "escape" => return None,
        _ if modifiers.control => {
            let text = crate::mode::text_key(key);
            let ch = if text == " " {
                '@'
            } else {
                text.chars().next()?
            };
            if text.chars().count() != 1 {
                return None;
            }
            let ch = ch.to_ascii_uppercase();
            match ch {
                '@'..='_' => Some(vec![ch as u8 & 0x1f]),
                '2' => Some(vec![0]),
                '3' => Some(vec![27]),
                '4' => Some(vec![28]),
                '5' => Some(vec![29]),
                '6' => Some(vec![30]),
                '7' => Some(vec![31]),
                '8' | '?' => Some(vec![127]),
                _ => None,
            }
        }
        _ if modifiers.alt && !modifiers.platform => {
            Some(crate::mode::text_key(key).as_bytes().to_vec())
        }
        _ => None,
    }?;
    if modifiers.alt && !(key.key == "tab" && modifiers.shift) {
        Some([vec![27], value].concat())
    } else {
        Some(value)
    }
}

pub(super) fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    // Embedded ESC must not be able to terminate bracketed paste and inject keys.
    let text = text.replace('\x1b', "");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\n").replace('\n', "\r").into_bytes()
    }
}
