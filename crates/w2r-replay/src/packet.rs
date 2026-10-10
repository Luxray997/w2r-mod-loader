//! Splits one slot's turn packet into commands. A packet is a run of opcode-prefixed records of
//! fixed size per opcode, from the two executors in `ProcessNetworkTurn` (docs/replay-research.md):
//! `0x49e260` (ops 05-18, 30) and `0x49e7b0` (19-23, 2f). The sets do not overlap apart from the
//! idle op 05. Opcode meanings are unnamed so far; this only finds the boundaries.

/// Size of the record starting at `data[0]`, or `None` if the opcode is unknown or the record
/// is cut off.
pub fn record_len(data: &[u8]) -> Option<usize> {
    let n = match *data.first()? {
        0x05 | 0x0c | 0x0d | 0x11 | 0x12 | 0x14 | 0x16 | 0x2f => 1,
        0x0b | 0x1a | 0x30 => 2,
        0x0f | 0x15 | 0x17 | 0x1f | 0x21 | 0x22 => 3,
        0x18 => 4,
        0x0e | 0x19 => 5,
        0x09 | 0x0a | 0x1b => 6,
        0x10 => 7,
        0x13 | 0x1d => 8,
        0x23 => 0x0d,
        0x1e => 0x12,
        // 5 bytes then a NUL-terminated string (the game reads at most 0x1c after the header).
        0x06 | 0x07 => {
            let body = data.get(5..)?;
            let nul = body.iter().take(0x1c).position(|&b| b == 0)?;
            5 + nul + 1
        }
        // 2 bytes, then n pairs.
        0x08 => 2 + 2 * usize::from(*data.get(1)?),
        _ => return None,
    };
    (data.len() >= n).then_some(n)
}

/// Every record in `data` as `(opcode, bytes)`. Stops at the first unknown or truncated record;
/// the second value is the number of bytes left unparsed (0 for a clean packet).
pub fn commands(data: &[u8]) -> (Vec<(u8, &[u8])>, usize) {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let Some(n) = record_len(&data[at..]) else {
            break;
        };
        out.push((data[at], &data[at..at + n]));
        at += n;
    }
    (out, data.len() - at)
}

/// Records that are not the idle op 05: what a player actually did.
pub fn actions(data: &[u8]) -> usize {
    commands(data)
        .0
        .iter()
        .filter(|(op, _)| *op != 0x05)
        .count()
}

/// What a player did on purpose: every record except the idle op 05 and the per-turn sync
/// record 18, which every packet carries.
pub fn player_actions(data: &[u8]) -> usize {
    commands(data)
        .0
        .iter()
        .filter(|(op, _)| *op != 0x05 && *op != 0x18)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_actions_skip_idle_and_sync_records() {
        let mut p = vec![0x05, 0x19, 1, 2, 3, 4];
        p.extend_from_slice(&[0x08, 2, 1, 2, 3, 4]);
        let (cmds, rest) = commands(&p);
        assert_eq!(rest, 0);
        assert!(cmds.iter().any(|(op, _)| *op == 0x08));
        assert_eq!(player_actions(&p), 1 + cmds.iter().filter(|(op, _)| *op == 0x19).count());
    }

    #[test]
    fn fixed_and_variable_records() {
        let mut p = vec![0x05, 0x19, 1, 2, 3, 4];
        p.extend_from_slice(&[0x06, 0, 0, 0, 0, b'h', b'i', 0]);
        p.extend_from_slice(&[0x08, 2, 1, 2, 3, 4]);
        let (cmds, rest) = commands(&p);
        assert_eq!(rest, 0);
        let ops: Vec<u8> = cmds.iter().map(|c| c.0).collect();
        assert_eq!(ops, [0x05, 0x19, 0x06, 0x08]);
        assert_eq!(cmds[2].1.len(), 8);
        assert_eq!(actions(&p), 3);
    }

    #[test]
    fn unknown_and_truncated_stop_the_scan() {
        assert_eq!(commands(&[0x05, 0xee, 1]).1, 2);
        assert_eq!(commands(&[0x19, 1, 2]).1, 3);
        assert_eq!(commands(&[0x06, 0, 0, 0, 0, b'a']).1, 6);
        assert_eq!(commands(&[]).1, 0);
    }
}
