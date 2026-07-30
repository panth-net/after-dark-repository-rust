//! The physical keyboard, read the way the Mac's `GetKeys` read it.
//!
//! Every module on the disk steers by polling the low-memory `KeyMap`: a bitmap
//! of which keys are *physically down right now*. That is a level, not a stream
//! of edges, and it is the difference this crate exists to close.
//!
//! A windowing library cannot answer that question on macOS. It knows only what
//! the AppKit event stream told it, and while Command is held the event stream
//! stops telling the truth — auto-repeat is suppressed and `keyUp:` is not
//! delivered for ordinary keys. Command is Fire in Lunatic Fringe, so the one
//! key the game needs most is the key that breaks the reporting for every other
//! key. The player used to paper over this with a timer that guessed, after
//! Command came up, which keys had probably been released while it was down.
//! A guess is the best you can do from inside the event stream.
//!
//! So step outside it. `CGEventSourceKeyState` reads the state the window server
//! maintains from the HID layer, underneath event routing entirely, which makes
//! it immune to every way the event stream can lie. It is also the honest
//! modern spelling of what the module is asking for: `GetKeys` on a real
//! Macintosh and this call on this one answer the same question.
//!
//! # Why the `unsafe` lives here
//!
//! The workspace sets `unsafe_code = "forbid"` and that is worth keeping. This
//! crate opts out for three `extern` declarations and nothing else: no state, no
//! pointers, no allocation, no lifetimes to get wrong. Every one of them takes
//! integers and returns integers. Confining it to a crate this small is what
//! lets the forbid stay in force across the emulator, the rasteriser and the
//! resource parser, where it is actually preventing something.
//!
//! The third is [`hold_screen`], which is here for that reason and not because
//! taking the screen is keyboard state: it is one more integer-in, integer-out
//! call to the window server, and the alternative was granting the player crate
//! the right to write `unsafe` for the rest of time.
//!
//! # Platforms
//!
//! macOS only, and on purpose. Every function returns [`None`] elsewhere, which
//! the caller reads as "ask the windowing library instead". Windows and Linux do
//! not suppress key reporting under a modifier, so there is no bug to fix there
//! and no reason to carry untested FFI for one.

/// Reads from the session's combined state — hardware plus anything an assistive
/// or automation tool has synthesised. The alternative, `kCGEventSourceStateHID`
/// (`1`), sees only real hardware, which would silently make the game
/// unplayable for anyone driving it through accessibility software.
#[cfg(target_os = "macos")]
const COMBINED_SESSION_STATE: i32 = 0;

/// `kCGEventFlagMaskAlphaShift` — the Caps Lock bit in `CGEventFlags`.
#[cfg(target_os = "macos")]
const FLAG_ALPHA_SHIFT: u64 = 0x0001_0000;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// `CGEventSourceKeyState(CGEventSourceStateID, CGKeyCode) -> bool`
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    /// `CGEventSourceFlagsState(CGEventSourceStateID) -> CGEventFlags`
    fn CGEventSourceFlagsState(state: i32) -> u64;
}

/// Values from HIToolbox's `SetSystemUIMode`. `kUIModeAllHidden` is the menu
/// bar and the Dock; the options are the system chords to switch off with them.
#[cfg(target_os = "macos")]
mod ui_mode {
    pub const NORMAL: u32 = 0;
    pub const ALL_HIDDEN: u32 = 3;
    /// `kUIOptionDisableHide` — Command-H.
    ///
    /// `kUIOptionDisableProcessSwitch` (`1 << 3`) is deliberately *not* set. It
    /// would take Command-Tab as well as Mission Control and the Spaces switches,
    /// which are one thing to the window server — and being unable to switch away
    /// from a screen saver is a worse bargain than the occasional stray switch.
    /// Leaving a running module is already two presses of Escape.
    pub const DISABLE_HIDE: u32 = 1 << 6;
}

#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// `SetSystemUIMode(SystemUIMode, SystemUIOptions) -> OSStatus`
    fn SetSystemUIMode(mode: u32, options: u32) -> i32;
}

/// Holds the whole screen until it is dropped.
///
/// A borderless window at display size *covers* the menu bar and the Dock. It
/// does not switch them off, and it does not stop the window server handing a
/// keystroke to something other than the game. This does both.
///
/// `SetSystemUIMode` is the C entry point to the same switch as
/// `NSApplicationPresentationOptions`, and that is exactly why it is the one
/// used: one more `extern` declaration in the crate that already exists to hold
/// them, rather than a new dependency and an `objc_msgSend` signature to get
/// wrong on two architectures.
///
/// **Switched off while this is held:** the menu bar, the Dock, and Command-H.
/// Two things are deliberately left *on*. Force quit, because
/// Command-Option-Escape is the way out if a module ever wedges. And process
/// switching, so Command-Tab still works: being unable to leave a screen saver
/// is a worse bargain than the occasional stray switch away from one.
///
/// **Not switchable off, by this or anything else:** Spotlight. Command-Space
/// is dispatched by the window server before any application is offered the
/// event; only an accessibility-trusted event tap can intercept it, which means
/// a permission prompt this project has no business showing. Lunatic Fringe's
/// own keyboard column pairs Fire on Command with Power Shield on Space, so that
/// limit is why the player offers `F` as a second Fire — the chord cannot be
/// blocked, so the game has to be playable without pressing it.
#[derive(Debug)]
pub struct ScreenHold(());

/// Take the screen. The caller keeps the value for as long as it wants it.
///
/// Restoring is a [`Drop`], so it survives an early return and an unwind out of
/// a module. Leaving somebody with no menu bar and no Dock because a module
/// returned an error would mean logging out to get them back.
#[must_use]
pub fn hold_screen() -> ScreenHold {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a C function taking two integers by value and returning one.
        // No pointers, no ownership, no thread affinity.
        unsafe { SetSystemUIMode(ui_mode::ALL_HIDDEN, ui_mode::DISABLE_HIDE) };
    }
    ScreenHold(())
}

impl Drop for ScreenHold {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: as above. Restores what `hold_screen` changed.
            unsafe { SetSystemUIMode(ui_mode::NORMAL, 0) };
        }
    }
}

/// Whether the key with this virtual key code is physically down.
///
/// The code is the same ADB-derived number the classic `KeyMap` is indexed by —
/// `0x37` is Command, `0x7B` is Left Arrow — so a caller that already has a
/// table of Mac key codes for the emulator can hand them straight over. That is
/// not a coincidence: macOS never renumbered them.
///
/// [`None`] means the platform has no answer, not that the key is up. Those are
/// very different things to a caller and must not collapse into `false`.
#[must_use]
pub fn key_down(mac_key_code: u8) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a C function taking two integers by value and returning a
        // `bool`, with no pointers, no ownership and no thread affinity. There is
        // no invariant available to break.
        Some(unsafe { CGEventSourceKeyState(COMBINED_SESSION_STATE, u16::from(mac_key_code)) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = mac_key_code;
        None
    }
}

/// Whether Caps Lock is *locked on*, which is not the same as held down.
///
/// Caps Lock is the one key on the keyboard whose state outlives the press, and
/// Lunatic Fringe uses exactly that property: the latch is what tells it the
/// player is at the controls rather than watching a screensaver. It never
/// arrives as a `keyDown:`, so no amount of event-stream cleverness can find it;
/// it is a flag, and this is where the flag is kept.
#[must_use]
pub fn caps_lock() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: as `key_down` — one integer in, one integer out.
        Some(unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) } & FLAG_ALPHA_SHIFT != 0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Whether this platform can answer at all, so a caller can pick its strategy
/// once instead of unwrapping an [`Option`] on every key of every frame.
#[must_use]
pub const fn available() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The calls return without trapping, and agree with themselves.
    ///
    /// What a test cannot do is press a key, so this deliberately asserts
    /// nothing about *which* keys are down — a test that did would pass or fail
    /// on what the person running it happened to be holding. Liveness is
    /// established at run time instead; see the player's `hid` field.
    #[test]
    fn the_keyboard_can_be_read_without_trapping() {
        for code in [0x37u8, 0x7B, 0x39, 0x00, 0xFF] {
            assert_eq!(key_down(code).is_some(), available(), "code {code:#04X}");
        }
        assert_eq!(caps_lock().is_some(), available());
    }

    /// Reading twice in a row gives the same answer, which is the property the
    /// player relies on when it polls once per frame.
    #[test]
    fn a_read_is_stable_across_calls() {
        // Command, specifically: the key the whole crate is here for.
        assert_eq!(key_down(0x37), key_down(0x37));
    }
}
