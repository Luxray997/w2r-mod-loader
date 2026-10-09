//! The pure half of map previews: the PUD parser and the tile-averaging renderer. No game or
//! operating-system access, so the host and plugins share one copy and it is tested anywhere.

pub mod mappreview;
pub mod pud;
