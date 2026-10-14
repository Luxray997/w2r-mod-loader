//! Becoming an observer in the middle of a match. A player who has been defeated can type `/obs`
//! to keep watching the game properly: the viewing system (switch player, the all-seeing view,
//! scoreboard, auto-director, camera follow) starts for them, and the others are told, so the
//! players send them their view and nobody sees their chat.
//!
//! Nothing here touches the simulation: a defeated player owns nothing, and the viewing system only
//! changes what this machine draws. The announcement is one small message of our own subsystem
//! (`'O'`) to every other machine that runs the mod. x86 only apart from the wire format.

use crate::peer;

/// The envelope's subsystem byte.
pub const SUBSYSTEM: u8 = b'O';
const PROTOCOL: u8 = 1;

/// A whole message: the envelope, the protocol, the player who now observes.
pub fn encode(player: u8) -> [u8; 7] {
    [peer::TYPE, peer::MAGIC[0], peer::MAGIC[1], peer::MAGIC[2], SUBSYSTEM, PROTOCOL, player]
}

/// The player in a message body (everything after the envelope).
pub fn decode(body: &[u8]) -> Option<u8> {
    match body {
        [PROTOCOL, player, ..] if *player < 8 => Some(*player),
        _ => None,
    }
}

/// True for `/obs` on its own (any case, surrounding spaces ignored).
pub fn is_command(text: &[u8]) -> bool {
    text.trim_ascii().eq_ignore_ascii_case(b"/obs")
}

#[cfg(target_arch = "x86")]
pub use live::{deliver, start_local};

#[cfg(target_arch = "x86")]
mod live {
    use w2r_abi::W2R_LOG_INFO;

    use super::*;
    use crate::host::Log;

    /// The local player types `/obs`.
    pub fn start_local() {
        if crate::replayplay::is_active() || crate::observer::local_is_observer() {
            crate::viewer::say("You are already observing");
            return;
        }
        let Some(me) = crate::camerashare::local_player() else { return };
        if !crate::camerashare::in_networked_match() {
            crate::viewer::say("Observing starts in a multiplayer match");
            return;
        }
        if !crate::camerashare::is_out(me) {
            crate::viewer::say("You can only start observing once you are defeated");
            return;
        }
        crate::observer::set_local_observer(true);
        crate::observer::set_match_observers(crate::observer::match_observers() | 1 << me);
        crate::camerashare::broadcast(&encode(me));
        crate::replayplay::live_start(me);
        crate::session::StartupLog.line(W2R_LOG_INFO, "observer", "now observing the match (/obs)");
        crate::viewer::say("Observing: [ ] player, Home all, Ins vision, End auto, ; follow");
    }

    /// Somebody else announced that they observe now.
    pub fn deliver(sender: u32, body: &[u8]) {
        let Some(claimed) = decode(body) else { return };
        // The announcement counts for the player the sender really is.
        if crate::camerashare::player_of_net(sender) != Some(claimed) {
            return;
        }
        crate::observer::set_match_observers(crate::observer::match_observers() | 1 << claimed);
        crate::session::StartupLog.line(
            W2R_LOG_INFO,
            "observer",
            &format!("player {claimed} now observes the match"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_round_trips() {
        for p in 0..8 {
            assert_eq!(decode(&encode(p)[5..]), Some(p));
        }
        assert_eq!(decode(&[1, 8]), None);
        assert_eq!(decode(&[2, 3]), None);
        assert_eq!(decode(&[1]), None);
    }

    #[test]
    fn the_command_is_recognised_alone_only() {
        assert!(is_command(b"/obs"));
        assert!(is_command(b"  /OBS "));
        assert!(!is_command(b"/obs me"));
        assert!(!is_command(b"obs"));
    }
}
