//! Chat for replays (docs/replay-research.md, "Chat"). In-game chat is not in the turn packets:
//! the game sends it straight between machines and shows it itself, so a recording of the
//! packets alone has none. Two places show a public chat line, and the host watches both without
//! touching the display function (the chat plugin hooks that one):
//!
//! - `ChatSendLocal` (0x49c4c0) sends the text to each human recipient and shows the local echo,
//!   unless the text is a `/` command or there is no recipient at all;
//! - `ChatReceive` (0x49e0c0) handles a message of type 0 from another machine and shows it if
//!   the sender is valid, in the sender mask and mapped to a player.
//!
//! After the game's function ran, the line it showed is recorded with the next turn to run
//! (`replayrec::note_chat`); playback calls the game's own display function at that turn.
//! Whispers (type 1, `/w`) are private and not recorded. x86 only (`cdecl`).

use core::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

/// How long the local echo stays on screen (what the game passes).
const LOCAL_DURATION_MS: u32 = 0x28;
/// A local line can be longer than what is sent; this much is kept.
const LOCAL_MAX: usize = 255;

type SendFn = unsafe extern "C" fn(*const u8);
type ReceiveFn = unsafe extern "C" fn(u32, *const u8, u32) -> u32;
type ValidFn = unsafe extern "C" fn(u32) -> u32;

static ORIG_SEND: AtomicUsize = AtomicUsize::new(0);
static ORIG_RECEIVE: AtomicUsize = AtomicUsize::new(0);
static ADDRS: OnceLock<Addrs> = OnceLock::new();

struct Addrs {
    local: usize,
    target_mode: usize,
    recipients: usize,
    net_mask: usize,
    slot_player: usize,
    sender_valid: usize,
}

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let get = |name: &str| symbols.address(name);
    let (Some(send), Some(receive), Some(valid)) = (
        get("ChatSendLocal"),
        get("ChatReceive"),
        get("ChatSenderValid"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "chat recording: symbols missing");
        return false;
    };
    let (Some(local), Some(target_mode), Some(recipients), Some(net_mask), Some(slot_player)) = (
        get("LocalPlayer"),
        get("ChatTargetMode"),
        get("ChatRecipientMask"),
        get("ChatNetMask"),
        get("NetSlotPlayer"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "chat recording: symbols missing");
        return false;
    };
    let _ = ADDRS.set(Addrs {
        local,
        target_mode,
        recipients,
        net_mask,
        slot_player,
        sender_valid: valid,
    });
    let mut ok = true;
    // SAFETY: each detour matches its target's cdecl signature (Ghidra, build 2818) and calls the
    // original with the same arguments.
    unsafe {
        ok &= hook(
            image,
            send,
            send_detour as *mut c_void,
            "ChatSendLocal",
            &ORIG_SEND,
            log,
        );
        ok &= hook(
            image,
            receive,
            receive_detour as *mut c_void,
            "ChatReceive",
            &ORIG_RECEIVE,
            log,
        );
    }
    ok
}

unsafe fn hook(
    image: &Image,
    target: usize,
    detour: *mut c_void,
    name: &str,
    original: &'static AtomicUsize,
    log: &dyn Log,
) -> bool {
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour,
            &format!("host:{name}"),
            |o| original.store(o as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {name} @ {target:#010x} (replay chat)"),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {name}: {e:?}"),
            );
            false
        }
    }
}

/// The bytes of a NUL-terminated string, at most `max` of them.
///
/// # Safety
/// `p` must point at readable memory up to the terminator or `max` bytes.
unsafe fn c_bytes(p: *const u8, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..max {
        let b = unsafe { *p.add(i) };
        if b == 0 {
            break;
        }
        out.push(b);
    }
    out
}

unsafe extern "C" fn send_detour(text: *const u8) {
    let original: SendFn = unsafe { core::mem::transmute(ORIG_SEND.load(Ordering::Acquire)) };
    // `/obs` starts observing (once defeated) and is not sent.
    let obs = guarded(|| crate::obsjoin::is_command(&unsafe { c_bytes(text, LOCAL_MAX) }))
        .unwrap_or(false);
    if obs {
        let _ = guarded(crate::obsjoin::start_local);
        return;
    }
    // `/plugins` goes out as the list of the player's plugins (docs/chat-commands.md).
    let replaced = guarded(|| {
        let bytes = unsafe { c_bytes(text, LOCAL_MAX) };
        if !crate::plugincmd::is_command(&bytes) {
            return None;
        }
        let mut line = crate::session::plugins_chat_line()?.into_bytes();
        line.truncate(LOCAL_MAX - 1);
        line.push(0);
        Some(line)
    })
    .ok()
    .flatten();
    if let Some(line) = replaced {
        return unsafe { send_detour_inner(original, line.as_ptr()) };
    }
    unsafe { send_detour_inner(original, text) }
}

unsafe fn send_detour_inner(original: SendFn, text: *const u8) {
    // An observer's chat goes to the other observers only.
    if crate::observer::local_is_observer() {
        unsafe { observer_send(original, text) };
        return;
    }
    // Read what the game is about to use before it runs: it changes the mask around the call.
    let seen = guarded(|| {
        let a = ADDRS.get()?;
        // SAFETY: the game passes a NUL-terminated line; the others are bytes in its data section.
        unsafe {
            let bytes = c_bytes(text, LOCAL_MAX);
            let (local, mode, recipients) = (
                *(a.local as *const u8),
                *(a.target_mode as *const u8),
                *(a.recipients as *const u8),
            );
            w2r_replay::chat::local_line_is_shown(&bytes, recipients, local)
                .then_some((local, mode, bytes))
        }
    })
    .ok()
    .flatten();
    unsafe { original(text) };
    if let Some((local, mode, bytes)) = seen {
        crate::replayrec::note_chat(local, mode, LOCAL_DURATION_MS, &bytes);
    }
}

/// Marks an observer's lines for the other observers.
const OBSERVER_TAG: &[u8] = b"[Obs] ";

/// Sends an observer's line to the observers in the match and nobody else: the recipient mask
/// is cut down to them, and the game's idea of who is local is the observer for the call (while an
/// observer watches a player, `LocalPlayer` is that player).
///
/// # Safety
/// `text` is the NUL-terminated line the game passed to `ChatSendLocal`.
unsafe fn observer_send(original: SendFn, text: *const u8) {
    let prepared = guarded(|| {
        let a = ADDRS.get()?;
        let real = crate::viewer::real();
        if real >= 8 {
            return None;
        }
        // SAFETY: bytes in the game's data section; the text is the game's NUL-terminated line.
        unsafe {
            let (mask_ptr, local_ptr) = (a.recipients as *mut u8, a.local as *mut u8);
            let (old_mask, old_local) = (*mask_ptr, *local_ptr);
            let mask = old_mask & crate::observer::match_observers() & !(1u8 << real);
            let mut line = OBSERVER_TAG.to_vec();
            line.extend_from_slice(&c_bytes(text, LOCAL_MAX - OBSERVER_TAG.len()));
            line.push(0);
            Some((mask, real, old_mask, old_local, line))
        }
    })
    .ok()
    .flatten();
    let Some((mask, real, old_mask, old_local, line)) = prepared else {
        unsafe { original(text) };
        return;
    };
    if mask == 0 {
        crate::viewer::say("No other observers to hear you");
        return;
    }
    let a = ADDRS.get();
    if let Some(a) = a {
        unsafe {
            *(a.recipients as *mut u8) = mask;
            *(a.local as *mut u8) = real;
        }
    }
    unsafe { original(line.as_ptr()) };
    if let Some(a) = a {
        unsafe {
            *(a.recipients as *mut u8) = old_mask;
            *(a.local as *mut u8) = old_local;
        }
    }
}

unsafe extern "C" fn receive_detour(net_id: u32, message: *const u8, length: u32) -> u32 {
    let original: ReceiveFn = unsafe { core::mem::transmute(ORIG_RECEIVE.load(Ordering::Acquire)) };
    // A player does not hear the observers, whatever the sender's machine did (an older build, a
    // modified one): their net slot is left out of the accepted senders for the call.
    let muted = guarded(|| unsafe { mute_observer(net_id) }).ok().flatten();
    if let Some(bit) = muted {
        let done = unsafe { original(net_id, message, length) };
        if let Some(a) = ADDRS.get() {
            unsafe { *(a.net_mask as *mut u8) |= bit };
        }
        return done;
    }
    let will_show = guarded(|| unsafe { received_line(net_id, message, length) })
        .ok()
        .flatten();
    let done = unsafe { original(net_id, message, length) };
    if let (Some((who, mode, bytes)), true) = (will_show, done != 0) {
        crate::replayrec::note_chat(who, mode, 0, &bytes);
    }
    done
}

/// When this machine plays and `net_id` belongs to an observer: clears that sender from the accepted
/// mask and returns the bit to put back.
///
/// # Safety
/// The addresses are the game's.
unsafe fn mute_observer(net_id: u32) -> Option<u8> {
    let a = ADDRS.get()?;
    if net_id >= 8 || crate::observer::local_is_observer() {
        return None;
    }
    let player = unsafe { *((a.slot_player + net_id as usize * 4) as *const u32) };
    if player >= 8 || crate::observer::match_observers() >> player & 1 == 0 {
        return None;
    }
    let bit = 1u8 << net_id;
    let mask = a.net_mask as *mut u8;
    let had = unsafe { *mask } & bit;
    unsafe { *mask &= !bit };
    (had != 0).then_some(bit)
}

/// The line `ChatReceive` will show for this message, if it shows one: `(player, mode, text)`.
///
/// # Safety
/// `message` must point at `length` readable bytes; the addresses are the game's.
unsafe fn received_line(net_id: u32, message: *const u8, length: u32) -> Option<(u8, u8, Vec<u8>)> {
    let a = ADDRS.get()?;
    if length < 2 || net_id >= 8 {
        return None;
    }
    let valid: ValidFn = unsafe { core::mem::transmute(a.sender_valid) };
    if unsafe { valid(net_id) } & 0xff == 0 {
        return None;
    }
    // Only as much of the message as the game was told is there is read.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            message,
            (length as usize).min(2 + w2r_replay::chat::SEND_MAX),
        )
    };
    let (mode, text) = w2r_replay::chat::received_text(bytes)?;
    let (mask, player) = unsafe {
        (
            *(a.net_mask as *const u8),
            *((a.slot_player + net_id as usize * 4) as *const u32),
        )
    };
    if mask >> net_id & 1 == 0 || player >= 8 {
        return None;
    }
    Some((player as u8, mode, text.to_vec()))
}
