//! The widget API (`W2rUi`, docs/abi.md) and the in-game plugin screen. Widgets are behind
//! [`Widgets`] so the plugin-facing rules are testable with a fake; `options.rs` binds them to
//! the game's own menu widgets.
//!
//! Each `W2rUi` call draws one row. Strings are at most 128 bytes of UTF-8; a bad string or a
//! null value pointer makes the call a no-op that reports "not pressed / not changed".

use core::ffi::c_void;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use w2r_abi::{W2rStr, W2rUi};

pub const MAX_TEXT: usize = 128;
/// Row height in the game's UI space (2160 high), the options screen's value.
pub const ROW_H: f32 = 90.0;
pub const HEADING_H: f32 = 120.0;

/// One row of widgets. `label` and friends each take the whole row.
pub trait Widgets {
    fn row(&self, height: f32, cols: i32);
    fn label(&self, text: &str);
    /// True when pressed this frame.
    fn button(&self, text: &str) -> bool;
    /// True when the value changed this frame.
    fn checkbox(&self, text: &str, value: &mut bool) -> bool;
    /// True when the value changed this frame. `min < max`.
    fn slider(&self, text: &str, value: &mut i32, min: i32, max: i32) -> bool;
    /// Greys out everything until `disable_end`.
    fn disable_begin(&self) {}
    fn disable_end(&self) {}
}

/// What a plugin's `on_ui` call sees behind `W2rUi.ctx`.
struct Frame<'a> {
    widgets: &'a dyn Widgets,
}

thread_local! {
    /// The one frame whose table may be used right now (its address), or 0. A `W2rUi` kept
    /// past `on_ui`, or used from another thread, no longer matches and every call is a no-op
    /// instead of a read through a dead pointer.
    static ACTIVE: Cell<usize> = const { Cell::new(0) };
}

/// Runs `f` with a table over `widgets`. The table is live only inside `f`; the frame behind
/// it never moves, and the previous active frame (if `f` nests) is restored on the way out.
pub fn with_frame<R>(widgets: &dyn Widgets, f: impl FnOnce(&W2rUi) -> R) -> R {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|a| a.set(self.0));
        }
    }
    let mut frame = Frame { widgets };
    let ctx: *mut Frame<'_> = &mut frame;
    let _restore = Restore(ACTIVE.with(|a| a.replace(ctx as usize)));
    let table = W2rUi {
        struct_size: size_of::<W2rUi>() as u32,
        ctx: ctx.cast(),
        label: cb::label,
        button: cb::button,
        checkbox: cb::checkbox,
        slider_int: cb::slider_int,
    };
    f(&table)
}

mod cb {
    use super::*;

    /// Only the frame that is live on this thread; anything else is refused.
    unsafe fn frame<'a>(ctx: *mut c_void) -> Option<&'a Frame<'a>> {
        if ctx.is_null() || ACTIVE.with(|a| a.get()) != ctx as usize {
            return None;
        }
        unsafe { ctx.cast::<Frame<'a>>().as_ref() }
    }

    /// Host code must not unwind into the plugin.
    fn run(f: impl FnOnce() -> i32) -> i32 {
        catch_unwind(AssertUnwindSafe(f)).unwrap_or(0)
    }

    unsafe fn text<'a>(s: W2rStr) -> Option<&'a str> {
        if s.len as usize > MAX_TEXT || (s.ptr.is_null() && s.len != 0) {
            return None;
        }
        if s.len == 0 {
            return Some("");
        }
        let bytes = unsafe { core::slice::from_raw_parts(s.ptr.cast::<u8>(), s.len as usize) };
        core::str::from_utf8(bytes).ok()
    }

    pub unsafe extern "C" fn label(ctx: *mut c_void, text: W2rStr) {
        run(|| {
            let (Some(f), Some(t)) = (unsafe { frame(ctx) }, unsafe { self::text(text) }) else {
                return 0;
            };
            f.widgets.row(ROW_H, 1);
            f.widgets.label(t);
            0
        });
    }

    pub unsafe extern "C" fn button(ctx: *mut c_void, text: W2rStr) -> i32 {
        run(|| {
            let (Some(f), Some(t)) = (unsafe { frame(ctx) }, unsafe { self::text(text) }) else {
                return 0;
            };
            f.widgets.row(ROW_H, 1);
            f.widgets.button(t) as i32
        })
    }

    pub unsafe extern "C" fn checkbox(ctx: *mut c_void, text: W2rStr, value: *mut i32) -> i32 {
        run(|| {
            let (Some(f), Some(t)) = (unsafe { frame(ctx) }, unsafe { self::text(text) }) else {
                return 0;
            };
            if value.is_null() {
                return 0;
            }
            let mut v = unsafe { *value } != 0;
            f.widgets.row(ROW_H, 1);
            if f.widgets.checkbox(t, &mut v) {
                unsafe { *value = v as i32 };
                1
            } else {
                0
            }
        })
    }

    pub unsafe extern "C" fn slider_int(
        ctx: *mut c_void,
        text: W2rStr,
        value: *mut i32,
        min: i32,
        max: i32,
    ) -> i32 {
        run(|| {
            let (Some(f), Some(t)) = (unsafe { frame(ctx) }, unsafe { self::text(text) }) else {
                return 0;
            };
            if value.is_null() || min >= max {
                return 0;
            }
            let mut v = unsafe { *value }.clamp(min, max);
            f.widgets.row(ROW_H, 2);
            if f.widgets.slider(t, &mut v, min, max) {
                unsafe { *value = v };
                1
            } else {
                0
            }
        })
    }
}

/// A plugin as the screen shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRow {
    pub id: String,
    pub version: String,
    /// `enabled` in state.json (takes effect at the next launch).
    pub enabled: bool,
    /// What the host did with it this session.
    pub status: Status,
    pub slow: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Active,
    Disabled(String),
    /// Enabled in state.json but refused before it got a slot, or not enabled.
    NotLoaded,
}

/// The host's own section at the top of the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRow {
    /// The simulation fingerprint of this session (docs/load-policy.md).
    pub fingerprint: [u8; 4],
    /// The plugins in it, by id.
    pub simulation: Vec<String>,
    /// `require_matching_plugins` (`host.json`); takes effect at once.
    pub strict: bool,
    /// The handshake's current lobby warnings, if any.
    pub lobby: Vec<String>,
}

pub const STRICT_LABEL: &str = "When hosting, remove players whose simulation plugins differ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Back,
    /// The user flipped a plugin's enabled flag.
    Toggle(usize),
    /// The user flipped `require_matching_plugins`.
    ToggleStrict,
}

/// Draws the host section and the plugin list. `draw_section` draws the plugin's own widgets
/// for an active row.
pub fn draw_plugin_screen(
    w: &dyn Widgets,
    host: &mut HostRow,
    rows: &mut [PluginRow],
    mut draw_section: impl FnMut(&dyn Widgets, usize),
) -> Action {
    let mut action = Action::None;
    w.row(HEADING_H, 1);
    w.label("Host");
    w.row(ROW_H, 1);
    w.label(&format!(
        "Simulation fingerprint: {} ({})",
        crate::fingerprint::hex(&host.fingerprint),
        if host.simulation.is_empty() {
            "no simulation-affecting plugins".to_owned()
        } else {
            host.simulation.join(", ")
        }
    ));
    w.row(ROW_H, 1);
    let mut strict = host.strict;
    if w.checkbox(STRICT_LABEL, &mut strict) {
        host.strict = strict;
        action = Action::ToggleStrict;
    }
    for line in &host.lobby {
        w.row(ROW_H, 1);
        w.label(&format!("Lobby: {line}"));
    }
    w.row(HEADING_H, 1);
    w.label(&format!("Plugins ({})", rows.len()));
    if rows.is_empty() {
        w.row(ROW_H, 1);
        w.label("No plugins installed. Use the w2rpm manager to add some.");
    }
    for (i, row) in rows.iter_mut().enumerate() {
        w.row(ROW_H, 2);
        w.label(&format!("{} {}", row.id, row.version));
        let mut enabled = row.enabled;
        if w.checkbox("Enabled (next launch)", &mut enabled) {
            row.enabled = enabled;
            action = Action::Toggle(i);
        }
        w.row(ROW_H, 1);
        let status = match &row.status {
            Status::Active if row.slow => "Status: running (slow callbacks)".to_owned(),
            Status::Active => "Status: running".to_owned(),
            Status::Disabled(reason) => format!("Status: disabled, {reason}"),
            Status::NotLoaded => "Status: not loaded this session (w2r.log says why)".to_owned(),
        };
        w.label(&status);
        if row.status == Status::Active {
            draw_section(w, i);
        }
    }
    w.row(HEADING_H, 1);
    if w.button("Back") {
        action = Action::Back;
    }
    action
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Records every call; `press` / `flip` / `set` script what the user does.
    #[derive(Default)]
    pub struct FakeWidgets {
        pub calls: RefCell<Vec<String>>,
        pub press: RefCell<Vec<String>>,
        pub flip: RefCell<Vec<String>>,
        pub set: RefCell<BTreeMap<String, i32>>,
    }

    impl Widgets for FakeWidgets {
        fn row(&self, height: f32, cols: i32) {
            self.calls.borrow_mut().push(format!("row {height} {cols}"));
        }
        fn label(&self, text: &str) {
            self.calls.borrow_mut().push(format!("label {text:?}"));
        }
        fn button(&self, text: &str) -> bool {
            self.calls.borrow_mut().push(format!("button {text:?}"));
            self.press.borrow().iter().any(|p| p == text)
        }
        fn checkbox(&self, text: &str, value: &mut bool) -> bool {
            self.calls
                .borrow_mut()
                .push(format!("checkbox {text:?} {value}"));
            if self.flip.borrow().iter().any(|p| p == text) {
                *value = !*value;
                true
            } else {
                false
            }
        }
        fn slider(&self, text: &str, value: &mut i32, min: i32, max: i32) -> bool {
            self.calls
                .borrow_mut()
                .push(format!("slider {text:?} {value} {min} {max}"));
            match self.set.borrow().get(text) {
                Some(&v) if v != *value => {
                    *value = v;
                    true
                }
                _ => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeWidgets;
    use super::*;

    fn s(text: &str) -> W2rStr {
        W2rStr {
            ptr: text.as_ptr().cast(),
            len: text.len() as u32,
        }
    }

    #[test]
    fn the_table_routes_each_widget_to_one_row() {
        let fake = FakeWidgets::default();
        fake.press.borrow_mut().push("Go".into());
        fake.flip.borrow_mut().push("Flag".into());
        fake.set.borrow_mut().insert("Vol".into(), 7);
        let mut flag = 0i32;
        let mut vol = 3i32;
        let mut untouched = 1i32;
        with_frame(&fake, |ui| unsafe {
            (ui.label)(ui.ctx, s("Hello"));
            assert_eq!((ui.button)(ui.ctx, s("Go")), 1);
            assert_eq!((ui.button)(ui.ctx, s("Stop")), 0);
            assert_eq!((ui.checkbox)(ui.ctx, s("Flag"), &mut flag), 1);
            assert_eq!(flag, 1);
            assert_eq!((ui.checkbox)(ui.ctx, s("Other"), &mut untouched), 0);
            assert_eq!(untouched, 1);
            assert_eq!((ui.slider_int)(ui.ctx, s("Vol"), &mut vol, 0, 10), 1);
            assert_eq!(vol, 7);
            assert_eq!(
                (ui.slider_int)(ui.ctx, s("Vol"), &mut vol, 0, 10),
                0,
                "already 7"
            );
        });
        let calls = fake.calls.borrow();
        assert_eq!(calls[0], "row 90 1");
        assert_eq!(calls[1], "label \"Hello\"");
        assert_eq!(calls[3], "button \"Go\"");
        assert_eq!(calls[7], "checkbox \"Flag\" false");
        assert_eq!(calls[11], "slider \"Vol\" 3 0 10");
    }

    #[test]
    fn bad_arguments_are_no_ops() {
        let fake = FakeWidgets::default();
        fake.press.borrow_mut().push("Go".into());
        let long = "x".repeat(MAX_TEXT + 1);
        let mut v = 5i32;
        with_frame(&fake, |ui| unsafe {
            (ui.label)(ui.ctx, s(&long));
            (ui.label)(
                ui.ctx,
                W2rStr {
                    ptr: core::ptr::null(),
                    len: 3,
                },
            );
            (ui.label)(
                ui.ctx,
                W2rStr {
                    ptr: [0xFFu8, 0xFE].as_ptr().cast(),
                    len: 2,
                },
            );
            assert_eq!((ui.button)(ui.ctx, s(&long)), 0);
            assert_eq!((ui.checkbox)(ui.ctx, s("x"), core::ptr::null_mut()), 0);
            assert_eq!((ui.slider_int)(ui.ctx, s("x"), &mut v, 5, 5), 0);
            assert_eq!(
                (ui.slider_int)(ui.ctx, s("x"), core::ptr::null_mut(), 0, 5),
                0
            );
            assert_eq!((ui.button)(core::ptr::null_mut(), s("Go")), 0);
            (ui.label)(ui.ctx, s(""));
        });
        assert_eq!(fake.calls.borrow().len(), 2, "{:?}", fake.calls.borrow());
    }

    #[test]
    fn a_table_kept_past_its_frame_is_inert() {
        let fake = FakeWidgets::default();
        fake.press.borrow_mut().push("Go".into());
        let kept = with_frame(&fake, |ui| {
            unsafe { assert_eq!((ui.button)(ui.ctx, s("Go")), 1) };
            *ui
        });
        unsafe { assert_eq!((kept.button)(kept.ctx, s("Go")), 0) };
        // A later frame does not revive the old table, and the old one cannot reach the new.
        with_frame(&fake, |ui| unsafe {
            assert_eq!((kept.button)(kept.ctx, s("Go")), 0);
            assert_eq!((ui.button)(ui.ctx, s("Go")), 1);
        });
        assert_eq!(fake.calls.borrow().len(), 4, "{:?}", fake.calls.borrow());
    }

    #[test]
    fn the_plugin_screen_lists_toggles_and_goes_back() {
        let fake = FakeWidgets::default();
        fake.flip.borrow_mut().push("Enabled (next launch)".into());
        let mut rows = vec![
            PluginRow {
                id: "a".into(),
                version: "1.0.0".into(),
                enabled: true,
                status: Status::Active,
                slow: false,
            },
            PluginRow {
                id: "b".into(),
                version: "2.0.0".into(),
                enabled: false,
                status: Status::NotLoaded,
                slow: false,
            },
            PluginRow {
                id: "c".into(),
                version: "0.1.0".into(),
                enabled: true,
                status: Status::Disabled("boom".into()),
                slow: true,
            },
        ];
        let mut host = HostRow {
            fingerprint: [0xaa, 0xbb, 0xcc, 0xdd],
            simulation: vec!["a".into(), "c".into()],
            strict: false,
            lobby: vec!["Player 2 runs different simulation plugins".into()],
        };
        let mut sections = Vec::new();
        let action = draw_plugin_screen(&fake, &mut host, &mut rows, |_, i| sections.push(i));
        // Every row's checkbox flipped; the last flip wins as the reported action.
        assert_eq!(action, Action::Toggle(2));
        assert!(!rows[0].enabled && rows[1].enabled && !rows[2].enabled);
        assert!(!host.strict, "not flipped");
        assert_eq!(sections, vec![0], "only active plugins draw their section");
        let calls = fake.calls.borrow();
        assert_eq!(calls[1], "label \"Host\"");
        assert_eq!(
            calls[3],
            "label \"Simulation fingerprint: aabbccdd (a, c)\""
        );
        assert_eq!(calls[5], format!("checkbox {STRICT_LABEL:?} false"));
        assert_eq!(
            calls[7],
            "label \"Lobby: Player 2 runs different simulation plugins\""
        );
        assert_eq!(calls[9], "label \"Plugins (3)\"");
        assert!(calls.iter().any(|c| c == "label \"Status: running\""));
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("label \"Status: not loaded"))
        );
        assert!(
            calls
                .iter()
                .any(|c| c == "label \"Status: disabled, boom\"")
        );
        drop(calls);

        let fake = FakeWidgets::default();
        fake.press.borrow_mut().push("Back".into());
        let mut host = HostRow {
            fingerprint: [0; 4],
            simulation: Vec::new(),
            strict: false,
            lobby: Vec::new(),
        };
        assert_eq!(
            draw_plugin_screen(&fake, &mut host, &mut [], |_, _| {}),
            Action::Back
        );
        let calls = fake.calls.borrow();
        assert!(calls.iter().any(|c| c.contains("No plugins installed")));
        assert!(
            calls
                .iter()
                .any(|c| c.contains("00000000 (no simulation-affecting plugins)"))
        );
        assert!(!calls.iter().any(|c| c.starts_with("label \"Lobby:")));
        drop(calls);

        // The strict checkbox is its own action.
        let fake = FakeWidgets::default();
        fake.flip.borrow_mut().push(STRICT_LABEL.into());
        assert_eq!(
            draw_plugin_screen(&fake, &mut host, &mut [], |_, _| {}),
            Action::ToggleStrict
        );
        assert!(host.strict);
    }
}
