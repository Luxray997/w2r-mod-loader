//! Replays (docs/replay-research.md): the game is lockstep, so a match is its start state plus
//! the command packets every slot sent each turn. This crate holds the `.w2rr` file format, the
//! streaming writer, the tolerant reader and a scanner that splits a packet into commands. Pure
//! Rust with no game or Windows calls, so everything is tested without the game.

pub mod format;
pub mod packet;
pub mod reader;
pub mod writer;

pub use format::{Footer, Header};
pub use reader::{Replay, Turn};
pub use writer::Writer;
