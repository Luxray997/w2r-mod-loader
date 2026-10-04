//! The hook registry (Phase 2 step 3): every inline hook and byte patch in the game, who owns
//! it, and the bytes it covers, so two patches never overlap. The engine underneath is MinHook
//! (`minhook-sys`, BSD-2); the registry is pure bookkeeping and is tested everywhere.
//!
//! Process-global, because the host installs its own hooks from the startup thread before any
//! plugin exists, and plugins add theirs from the main thread later.

use std::sync::{Mutex, MutexGuard};

use w2r_abi::W2rHandle;

/// Bytes an inline hook overwrites at the target (a `jmp rel32`).
pub const PATCH_LEN: usize = 5;
/// Bytes an inline hook *owns* in the registry. MinHook copies whole instructions into the
/// trampoline until it has covered the patch, so up to one maximal x86 instruction (14 more
/// bytes) past the patch is executed from the trampoline; a hook placed inside those bytes
/// would corrupt it. Overlap checks therefore reserve this span, not just the patch.
pub const HOOK_SPAN: usize = PATCH_LEN + 14;
/// docs/abi.md: at most 64 hooks per plugin.
pub const MAX_PER_PLUGIN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Host,
    /// Registry slot index of the plugin.
    Plugin(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookError {
    /// The target is not inside an executable section of the game module.
    NotCode,
    /// Overlaps an existing hook or patch; carries its label.
    Conflict(String),
    /// The plugin already has `MAX_PER_PLUGIN` hooks.
    Limit,
    /// No such handle for this owner.
    NotFound,
    /// MinHook refused; carries its `MH_STATUS`.
    Engine(i32),
    /// The caller's `publish` faulted (a plugin's bad `original_out`); nothing was installed.
    Publish,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub handle: W2rHandle,
    pub owner: Owner,
    pub start: usize,
    pub len: usize,
    pub label: String,
    /// True when MinHook holds a hook at `start` that must be removed with the entry.
    pub engine: bool,
}

impl Entry {
    fn overlaps(&self, start: usize, len: usize) -> bool {
        start < self.start + self.len && self.start < start + len
    }
}

/// Bookkeeping only; see [`Hooks`] for the version that drives the engine.
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<Entry>,
    next: W2rHandle,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn count(&self, owner: Owner) -> usize {
        self.entries.iter().filter(|e| e.owner == owner).count()
    }

    /// Overlap and per-plugin limit, without recording anything.
    pub fn check(&self, owner: Owner, start: usize, len: usize) -> Result<(), HookError> {
        if let Some(e) = self.entries.iter().find(|e| e.overlaps(start, len)) {
            return Err(HookError::Conflict(e.label.clone()));
        }
        if matches!(owner, Owner::Plugin(_)) && self.count(owner) >= MAX_PER_PLUGIN {
            return Err(HookError::Limit);
        }
        Ok(())
    }

    /// Records a range. `engine` says whether MinHook holds a hook there.
    pub fn record(
        &mut self,
        owner: Owner,
        start: usize,
        len: usize,
        label: &str,
        engine: bool,
    ) -> Result<W2rHandle, HookError> {
        self.check(owner, start, len)?;
        self.next += 1;
        self.entries.push(Entry {
            handle: self.next,
            owner,
            start,
            len,
            label: label.to_owned(),
            engine,
        });
        Ok(self.next)
    }

    /// Forgets one entry. Only its owner may release it.
    pub fn release(&mut self, owner: Owner, handle: W2rHandle) -> Result<Entry, HookError> {
        let i = self
            .entries
            .iter()
            .position(|e| e.handle == handle && e.owner == owner)
            .ok_or(HookError::NotFound)?;
        Ok(self.entries.remove(i))
    }

    pub fn release_all(&mut self, owner: Owner) -> Vec<Entry> {
        let (mine, rest): (Vec<_>, Vec<_>) = self.entries.drain(..).partition(|e| e.owner == owner);
        self.entries = rest;
        mine
    }
}

/// The registry plus the engine. One per process; get it with [`global`].
#[derive(Debug, Default)]
pub struct Hooks {
    pub registry: Registry,
}

static GLOBAL: Mutex<Hooks> = Mutex::new(Hooks {
    registry: Registry {
        entries: Vec::new(),
        next: 0,
    },
});

pub fn global() -> MutexGuard<'static, Hooks> {
    GLOBAL.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hooks {
    /// Reserves bytes the host patches directly (no trampoline), so nothing else may hook them.
    pub fn reserve(
        &mut self,
        owner: Owner,
        start: usize,
        len: usize,
        label: &str,
    ) -> Result<W2rHandle, HookError> {
        self.registry.record(owner, start, len, label, false)
    }

    /// Installs an inline hook at `target`, which must be code in `image`. `publish` receives
    /// the trampoline to the original function *before* the hook is enabled, so a detour that
    /// runs immediately can already call through it.
    ///
    /// # Safety
    /// `detour` must be a function with the target's exact signature and calling convention
    /// that stays valid for the life of the hook; it may be called from any thread the game
    /// uses, as soon as this returns.
    #[cfg(windows)]
    pub unsafe fn install(
        &mut self,
        owner: Owner,
        image: &crate::pe::Image,
        target: usize,
        detour: *mut core::ffi::c_void,
        label: &str,
        publish: impl FnOnce(*mut core::ffi::c_void),
    ) -> Result<W2rHandle, HookError> {
        if !image.is_code(target, PATCH_LEN) {
            return Err(HookError::NotCode);
        }
        self.registry.check(owner, target, HOOK_SPAN)?;
        let original = unsafe { engine::create(target, detour)? };
        // `publish` may write to the caller's memory (a plugin's `original_out`). The caller
        // holds the registry lock while in here, and a fault would abandon this frame with the
        // lock held forever, so the write gets its own guard.
        if crate::guard::guarded(|| publish(original)).is_err() {
            let _ = engine::remove(target);
            return Err(HookError::Publish);
        }
        if let Err(e) = engine::enable(target) {
            let _ = engine::remove(target);
            return Err(e);
        }
        self.registry.record(owner, target, HOOK_SPAN, label, true)
    }

    /// Removes one hook or reservation. Only its owner may.
    pub fn remove(&mut self, owner: Owner, handle: W2rHandle) -> Result<(), HookError> {
        let entry = self.registry.release(owner, handle)?;
        Self::tear_down(&entry)
    }

    /// Removes everything `owner` holds (a disabled plugin). Returns the labels removed.
    pub fn remove_all(&mut self, owner: Owner) -> Vec<String> {
        self.registry
            .release_all(owner)
            .into_iter()
            .map(|e| {
                let _ = Self::tear_down(&e);
                e.label
            })
            .collect()
    }

    fn tear_down(entry: &Entry) -> Result<(), HookError> {
        if !entry.engine {
            return Ok(());
        }
        #[cfg(windows)]
        {
            engine::remove(entry.start)
        }
        #[cfg(not(windows))]
        {
            Ok(())
        }
    }
}

#[cfg(windows)]
pub mod engine {
    use super::HookError;
    use core::ffi::c_void;
    use std::sync::Once;

    static INIT: Once = Once::new();
    static mut INIT_STATUS: i32 = minhook_sys::MH_OK;

    pub fn ensure_initialized() -> Result<(), HookError> {
        INIT.call_once(|| unsafe { INIT_STATUS = minhook_sys::MH_Initialize() });
        // SAFETY: written once inside the Once, read-only after.
        match unsafe { INIT_STATUS } {
            minhook_sys::MH_OK => Ok(()),
            s => Err(HookError::Engine(s)),
        }
    }

    /// Creates the hook (disabled) and returns the trampoline.
    ///
    /// # Safety
    /// See [`super::Hooks::install`].
    pub unsafe fn create(target: usize, detour: *mut c_void) -> Result<*mut c_void, HookError> {
        ensure_initialized()?;
        let mut original: *mut c_void = core::ptr::null_mut();
        let st =
            unsafe { minhook_sys::MH_CreateHook(target as *mut c_void, detour, &mut original) };
        if st != minhook_sys::MH_OK {
            return Err(HookError::Engine(st));
        }
        Ok(original)
    }

    pub fn enable(target: usize) -> Result<(), HookError> {
        match unsafe { minhook_sys::MH_EnableHook(target as *mut c_void) } {
            minhook_sys::MH_OK => Ok(()),
            s => Err(HookError::Engine(s)),
        }
    }

    pub fn remove(target: usize) -> Result<(), HookError> {
        let target = target as *mut c_void;
        unsafe { minhook_sys::MH_DisableHook(target) };
        match unsafe { minhook_sys::MH_RemoveHook(target) } {
            minhook_sys::MH_OK => Ok(()),
            s => Err(HookError::Engine(s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_detected_in_both_directions_and_adjacent_is_fine() {
        let mut r = Registry::new();
        let h = r.record(Owner::Host, 0x1000, 5, "a", false).unwrap();
        assert_eq!(h, 1);
        assert_eq!(
            r.check(Owner::Host, 0x1004, 5),
            Err(HookError::Conflict("a".into()))
        );
        assert_eq!(
            r.check(Owner::Host, 0x0FFC, 5),
            Err(HookError::Conflict("a".into()))
        );
        assert_eq!(
            r.check(Owner::Host, 0x0FFF, 10),
            Err(HookError::Conflict("a".into()))
        );
        assert_eq!(r.check(Owner::Plugin(0), 0x1005, 5), Ok(()));
        assert_eq!(r.check(Owner::Plugin(0), 0x0FFB, 5), Ok(()));
    }

    #[test]
    fn plugins_are_limited_hosts_are_not_and_release_frees_the_slot() {
        let mut r = Registry::new();
        for i in 0..MAX_PER_PLUGIN {
            r.record(Owner::Plugin(7), 0x10000 + i * 8, 5, "p", true)
                .unwrap();
        }
        assert_eq!(r.check(Owner::Plugin(7), 0x20000, 5), Err(HookError::Limit));
        assert_eq!(r.check(Owner::Plugin(8), 0x20000, 5), Ok(()));
        for i in 0..MAX_PER_PLUGIN {
            r.record(Owner::Host, 0x30000 + i * 8, 5, "h", true)
                .unwrap();
        }
        assert_eq!(r.check(Owner::Host, 0x40000, 5), Ok(()));

        assert_eq!(
            r.release(Owner::Plugin(8), 1),
            Err(HookError::NotFound),
            "not the owner"
        );
        let e = r.release(Owner::Plugin(7), 1).unwrap();
        assert_eq!((e.start, e.engine), (0x10000, true));
        assert_eq!(r.check(Owner::Plugin(7), 0x20000, 5), Ok(()));
        assert_eq!(
            r.release(Owner::Plugin(7), 1),
            Err(HookError::NotFound),
            "already gone"
        );

        let gone = r.release_all(Owner::Plugin(7));
        assert_eq!(gone.len(), MAX_PER_PLUGIN - 1);
        assert_eq!(r.count(Owner::Plugin(7)), 0);
        assert_eq!(r.count(Owner::Host), MAX_PER_PLUGIN);
        assert_eq!(r.entries().len(), MAX_PER_PLUGIN);
    }

    #[test]
    fn handles_never_repeat() {
        let mut r = Registry::new();
        let a = r.record(Owner::Host, 0x1000, 5, "a", false).unwrap();
        r.release(Owner::Host, a).unwrap();
        let b = r.record(Owner::Host, 0x1000, 5, "b", false).unwrap();
        assert_ne!(a, b);
    }

    /// The engine for real: hook a function in this test binary, call through the trampoline,
    /// remove the hook, and see the original behaviour return.
    #[cfg(windows)]
    #[test]
    fn minhook_installs_calls_through_and_removes() {
        use crate::pe::Image;
        use core::ffi::c_void;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

        #[inline(never)]
        extern "C" fn add_one(x: i32) -> i32 {
            std::hint::black_box(x) + 1
        }
        #[inline(never)]
        extern "C" fn add_two(x: i32) -> i32 {
            std::hint::black_box(x) + 2
        }
        static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn detour(x: i32) -> i32 {
            let original: extern "C" fn(i32) -> i32 =
                unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
            original(x) + 100
        }
        let call = |x: i32| -> i32 {
            let f: extern "C" fn(i32) -> i32 = std::hint::black_box(add_one);
            f(x)
        };

        let base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
        let image = Image::from_module(base).unwrap();
        let target = add_one as extern "C" fn(i32) -> i32 as usize;
        assert_eq!(call(1), 2);

        let publish = |o: *mut c_void| ORIGINAL.store(o as usize, Ordering::Release);
        let mut hooks = Hooks::default();
        let handle = unsafe {
            hooks.install(
                Owner::Plugin(3),
                &image,
                target,
                detour as *mut c_void,
                "test:add_one",
                publish,
            )
        }
        .expect("install");
        assert_eq!(
            call(1),
            102,
            "detour runs and reaches the original through the trampoline"
        );

        assert_eq!(
            unsafe {
                hooks.install(
                    Owner::Plugin(4),
                    &image,
                    target + 2,
                    detour as *mut c_void,
                    "x",
                    publish,
                )
            },
            Err(HookError::Conflict("test:add_one".into()))
        );
        static NOT_CODE: [u8; 8] = [0xC3; 8];
        assert_eq!(
            unsafe {
                hooks.install(
                    Owner::Host,
                    &image,
                    NOT_CODE.as_ptr() as usize,
                    detour as *mut c_void,
                    "d",
                    publish,
                )
            },
            Err(HookError::NotCode)
        );

        // A publish that faults leaves nothing behind: no hook, no entry, and the lock-holding
        // caller gets a clean error instead of an abandoned frame. The target is hookable
        // again afterwards, which proves the engine hook was removed.
        let target2 = add_two as extern "C" fn(i32) -> i32 as usize;
        assert_eq!(
            unsafe {
                hooks.install(
                    Owner::Plugin(5),
                    &image,
                    target2,
                    detour as *mut c_void,
                    "faulting publish",
                    |o| {
                        // Unmapped and non-null: the debug build's null check would be a
                        // non-unwinding panic (abort), not the hardware fault we test.
                        let p: *mut *mut c_void =
                            std::hint::black_box(core::ptr::without_provenance_mut(8));
                        p.write_volatile(o);
                    },
                )
            },
            Err(HookError::Publish)
        );
        assert_eq!(hooks.registry.count(Owner::Plugin(5)), 0);
        let h2 = unsafe {
            hooks.install(
                Owner::Plugin(5),
                &image,
                target2,
                detour as *mut c_void,
                "add_two",
                publish,
            )
        }
        .expect("target free again");
        hooks.remove(Owner::Plugin(5), h2).unwrap();

        assert_eq!(
            hooks.remove(Owner::Plugin(4), handle),
            Err(HookError::NotFound)
        );
        hooks.remove(Owner::Plugin(3), handle).unwrap();
        assert_eq!(call(1), 2, "original bytes restored");

        let handle = unsafe {
            hooks.install(
                Owner::Plugin(3),
                &image,
                target,
                detour as *mut c_void,
                "again",
                publish,
            )
        }
        .unwrap();
        assert_eq!(call(5), 106);
        assert_eq!(hooks.remove_all(Owner::Plugin(3)), vec!["again".to_owned()]);
        assert_eq!(call(5), 6);
        let _ = handle;
    }
}
