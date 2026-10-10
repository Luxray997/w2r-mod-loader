//! The rules that decide which chat lines the game shows, so the recorder keeps exactly those
//! (docs/replay-research.md, "Chat"). Pure, tested without the game.

/// The game sends at most this much chat text.
pub const SEND_MAX: usize = 0x50;

/// Whether the local echo of a sent line is shown: a line starting with `/` is a command, and
/// without anyone besides `local` in the recipient mask nothing is sent or shown.
pub fn local_line_is_shown(text: &[u8], recipients: u8, local: u8) -> bool {
    !text.is_empty() && text[0] != b'/' && recipients & !(1u8 << (local & 7)) != 0
}

/// A public chat message from another machine: byte 0 is the type (0), byte 1 the target mode,
/// then the text, NUL-terminated within the first 0x50 bytes. Returns the mode and the text
/// (without the terminator), or `None` if the game would drop it (no terminator, or empty).
pub fn received_text(message: &[u8]) -> Option<(u8, &[u8])> {
    if message.len() < 2 {
        return None;
    }
    let region = &message[2..];
    let region = &region[..region.len().min(SEND_MAX)];
    let end = region.iter().position(|&b| b == 0)?;
    (end > 0).then(|| (message[1], &region[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_line_is_shown_unless_it_is_a_command_or_nobody_is_listening() {
        assert!(local_line_is_shown(b"hello", 0b0000_0010, 0));
        assert!(
            !local_line_is_shown(b"/ignore bob", 0b0000_0010, 0),
            "a command"
        );
        assert!(!local_line_is_shown(b"", 0b0000_0010, 0), "empty");
        assert!(
            !local_line_is_shown(b"hello", 0b0000_0001, 0),
            "only our own bit"
        );
        assert!(local_line_is_shown(b"hello", 0b0000_0101, 2));
    }

    #[test]
    fn a_received_message_needs_a_terminated_non_empty_text() {
        assert_eq!(received_text(&[0, 2, b'h', b'i', 0]), Some((2, &b"hi"[..])));
        assert_eq!(
            received_text(&[0, 3, b'h', b'i', 0, b'x']),
            Some((3, &b"hi"[..])),
            "stops at the NUL"
        );
        assert_eq!(received_text(&[0, 2, b'h', b'i']), None, "no terminator");
        assert_eq!(received_text(&[0, 2, 0]), None, "empty text");
        assert_eq!(received_text(&[0]), None, "no mode byte");
        // the terminator must be inside the first 0x50 bytes
        let mut long = vec![0u8, 0];
        long.extend(std::iter::repeat(b'a').take(0x50));
        long.push(0);
        assert_eq!(received_text(&long), None);
        long[2 + 0x4f] = 0;
        assert_eq!(received_text(&long).map(|(_, t)| t.len()), Some(0x4f));
    }
}
