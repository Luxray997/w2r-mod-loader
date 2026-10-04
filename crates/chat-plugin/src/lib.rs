//! w2rpm chat plugin: a port of wc2r-plus `features/chat.cpp`, in slices (docs/chat-plugin.md).
//! Slice 1: capture the game's in-match messages and draw a scrollable history (fade, backlog,
//! shown while paused or typing). `model` is pure and tested everywhere; the game-facing layer is
//! 32-bit Windows only, like the game.

pub mod model;

#[cfg(target_arch = "x86")]
mod plugin;
