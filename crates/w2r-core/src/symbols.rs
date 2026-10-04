//! Named game functions and globals for the supported build (Phase 2 step 2). The table is
//! generated into `target/symbols.rs`; this resolves it against the live load address and hands
//! plugins opaque handles for `symbol_lookup`/`symbol_read`/`hook_install`.

use w2r_abi::W2rHandle;

use crate::target::{PREFERRED_IMAGE_BASE, Symbol, symbols::SYMBOLS};

/// Resolver for one load address of the game module.
#[derive(Clone, Copy, Debug)]
pub struct Symbols {
    base: usize,
}

impl Symbols {
    pub fn new(base: usize) -> Self {
        Symbols { base }
    }

    pub fn base(&self) -> usize {
        self.base
    }

    pub fn get(name: &str) -> Option<&'static Symbol> {
        SYMBOLS
            .binary_search_by(|s| s.name.cmp(name))
            .ok()
            .map(|i| &SYMBOLS[i])
    }

    /// Plugin-facing handle: table index + 1, so 0 stays invalid.
    pub fn handle(name: &str) -> Option<W2rHandle> {
        SYMBOLS
            .binary_search_by(|s| s.name.cmp(name))
            .ok()
            .map(|i| i as W2rHandle + 1)
    }

    pub fn by_handle(handle: W2rHandle) -> Option<&'static Symbol> {
        SYMBOLS.get(handle.checked_sub(1)? as usize)
    }

    pub fn address_of(&self, sym: &Symbol) -> usize {
        self.base + (sym.static_addr as usize - PREFERRED_IMAGE_BASE)
    }

    pub fn address(&self, name: &str) -> Option<usize> {
        Self::get(name).map(|s| self.address_of(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{Kind, symbols::COUNT};

    #[test]
    fn table_is_sorted_unique_and_inside_the_image() {
        assert_eq!(SYMBOLS.len(), COUNT);
        for w in SYMBOLS.windows(2) {
            assert!(w[0].name < w[1].name, "{} then {}", w[0].name, w[1].name);
        }
        for s in SYMBOLS {
            let rva = s.static_addr as usize - PREFERRED_IMAGE_BASE;
            assert!(
                rva < crate::target::EXPECTED_SIZE_OF_IMAGE as usize,
                "{}",
                s.name
            );
            assert!(!s.c_type.is_empty(), "{}", s.name);
        }
    }

    #[test]
    fn symbols_the_host_itself_relies_on_exist() {
        for name in [
            "BeginNamedWindow",
            "TransitionGameState",
            "ProcessNetworkTurn",
            "GetNextPeerPacket",
            "DisplayChatOrGameMessage",
            "SendComposedChat",
            "StringAssign",
            "MpLobbySendChat",
        ] {
            let s = Symbols::get(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(s.kind, Kind::Function, "{name}");
        }
        for name in ["NetSession", "MpLobbyChatInput"] {
            let s = Symbols::get(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(s.kind, Kind::Object, "{name}");
        }
    }

    #[test]
    fn lookup_relocates_and_handles_round_trip() {
        let syms = Symbols::new(0x1000_0000);
        let s = Symbols::get("ProcessNetworkTurn").unwrap();
        assert_eq!(s.static_addr, 0x0049_ea20);
        assert_eq!(syms.address_of(s), 0x1000_0000 + 0x9_ea20);
        assert_eq!(syms.address("ProcessNetworkTurn"), Some(0x1009_ea20));
        assert_eq!(syms.address("NoSuchThing"), None);

        let h = Symbols::handle("ProcessNetworkTurn").unwrap();
        assert_ne!(h, 0);
        assert_eq!(Symbols::by_handle(h).unwrap().name, "ProcessNetworkTurn");
        assert!(Symbols::by_handle(0).is_none());
        assert!(Symbols::by_handle(COUNT as W2rHandle + 1).is_none());
        assert_eq!(Symbols::handle("zzz"), None);
    }
}
