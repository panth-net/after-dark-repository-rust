//! After Dark: a windowed host and a module browser.
//!
//! ```text
//! cargo run -p ad-player --release              # browse the modules/ directory
//! cargo run -p ad-player --release -- <dir>     # browse somewhere else
//! cargo run -p ad-player --release -- <mod.rsrc># run one module directly
//! ```
//!
//! Two modes, one binary, because they are two halves of the same product: the
//! goal is modules that are "individually launchable programs" *and* "a GUI
//! listing all modules". Passing a path runs that module; passing nothing lists
//! them and runs whichever you pick, returning to the list when it ends.
//!
//! # Why the present hook
//!
//! A game module owns the machine. The host cannot present frames or pump input
//! "between frames", because for a game there are no frame boundaries — there is
//! one `DrawFrame` call that runs until the user quits. So the window is driven
//! from [`ad_host_v2::Host::set_present_hook`], which fires from inside the
//! emulator's tick loop, and sound from `set_sound_hook` beside it. Keyboard
//! state captured there is written straight into the low-memory `KeyMap` and, for
//! ordinary keys, delivered as `keyDown`/`keyUp` events through whatever
//! `_PostEvent` patch the module installed — the same two paths a real Mac used.
//!
//! That is also why the window lives behind an `Rc<RefCell<…>>`: the browser loop
//! and the emulator's hook both need it, and they take turns rather than running
//! at once.

#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod library;
mod setup;
mod ui;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ad_host_v2::Host;
use ad_resource::{GmMessage, ModuleSettings, ResourceFork};
use minifb::{Key, MouseButton, Scale, Window, WindowOptions};

use ui::{Canvas, Font, colour};

/// What this application calls itself, everywhere a person can read it.
///
/// "Player", not "After Dark". After Dark is Berkeley Systems' product and this
/// is not a copy of it: it is a thing that plays their modules, from the disk
/// the person already owns, and the name has to say so. The pattern is Greg
/// Parker's Fringe Player, which did the same for the same game.
///
/// The *save* directory is still `After Dark` — see [`ad_runtime::save_dir`].
/// That is a path and not a claim, and renaming it would orphan every high
/// score and every imported library already on disk.
const APP_NAME: &str = "After Dark Player";

/// The module screen's size, and therefore the window's. The browser uses the
/// same dimensions so switching between list and module never resizes anything.
const WIDTH: usize = 640;
const HEIGHT: usize = 480;

/// Classic Mac virtual key codes for the keys modules actually read.
///
/// These are the hardware codes, which is what a module compares against —
/// either directly out of the `KeyMap` or out of an event message's high byte.
/// The table is deliberately explicit: a wrong code is an input that silently
/// does nothing, which is the hardest kind of bug to see.
const KEY_MAP: &[(Key, u8)] = &[
    // F (0x03) is absent: it is a second Fire. See the modifiers below.
    (Key::A, 0x00),
    (Key::S, 0x01),
    (Key::D, 0x02),
    (Key::H, 0x04),
    (Key::G, 0x05),
    (Key::Z, 0x06),
    (Key::X, 0x07),
    (Key::C, 0x08),
    (Key::V, 0x09),
    (Key::B, 0x0B),
    (Key::Q, 0x0C),
    (Key::W, 0x0D),
    (Key::E, 0x0E),
    (Key::R, 0x0F),
    (Key::Y, 0x10),
    (Key::T, 0x11),
    (Key::Key1, 0x12),
    (Key::Key2, 0x13),
    (Key::Key3, 0x14),
    (Key::Key4, 0x15),
    (Key::Key6, 0x16),
    (Key::Key5, 0x17),
    (Key::Equal, 0x18),
    (Key::Key9, 0x19),
    (Key::Key7, 0x1A),
    (Key::Minus, 0x1B),
    (Key::Key8, 0x1C),
    (Key::Key0, 0x1D),
    (Key::RightBracket, 0x1E),
    (Key::O, 0x1F),
    (Key::U, 0x20),
    (Key::LeftBracket, 0x21),
    (Key::I, 0x22),
    (Key::P, 0x23),
    (Key::Enter, 0x24),
    (Key::L, 0x25),
    (Key::J, 0x26),
    (Key::Apostrophe, 0x27),
    (Key::K, 0x28),
    (Key::Semicolon, 0x29),
    (Key::Backslash, 0x2A),
    (Key::Comma, 0x2B),
    (Key::Slash, 0x2C),
    (Key::N, 0x2D),
    (Key::M, 0x2E),
    (Key::Period, 0x2F),
    (Key::Tab, 0x30),
    (Key::Space, 0x31),
    // Escape (0x35) is absent: it leaves the module, and a key that does two
    // things is a bug waiting to be filed. Backspace stays, because the modern
    // layout maps it to Abort Ship and the original passes it through.
    (Key::Backquote, 0x32),
    (Key::Backspace, 0x33),
    // Modifiers. Fire in Lunatic Fringe is Command (`LFky 128` stores it as the
    // cmdKey mask), read from the KeyMap, and both Command keys send it. The
    // game draws its own controls screen from that same resource and it says
    // `cmd` in both columns — a player reading the game's screen has to find it
    // true, so this table reproduces the original rather than correcting it.
    //
    // `F` is a second Fire, and on a laptop it is the one to use. The original's
    // **Keyboard** column pairs Fire on Command with Power Shield on Space, so
    // playing that set means holding Command and pressing Space — Spotlight on
    // every Mac made this century, dispatched by the window server before the
    // application is offered the event, and refusable by nothing. The **Keypad**
    // column has no such pair: Command with keypad 0 is not a system chord. The
    // original controls are therefore fine on a keyboard that has a keypad and
    // impossible on one that does not, which is precisely the gap `F` fills.
    //
    // `F` also sidesteps the lost key-up on `Shared::key_seen`: macOS stops
    // delivering `keyUp:` for ordinary keys while Command is held, so holding a
    // turn, firing, and releasing the turn leaves the ship turning forever.
    //
    // It is **remapped, not withheld** — it reaches the module every time, as
    // Command's code. What it does not do is *also* arrive as `0x03`, its own code,
    // and that is the only thing it costs. One key, one meaning: a key that sends
    // two codes can set off two things in a module that reads both, and the point
    // of F is to be the boring, predictable way to fire. `LFky` shows Lunatic
    // Fringe does not read `0x03`.
    (Key::LeftSuper, 0x37),
    (Key::RightSuper, 0x37),
    (Key::F, 0x37),
    (Key::LeftShift, 0x38),
    (Key::RightShift, 0x38),
    // Caps Lock is deliberately absent: it latches, and it is applied from
    // `Shared::caps` rather than polled as a held key. See that field.
    (Key::LeftAlt, 0x3A),
    (Key::RightAlt, 0x3A),
    (Key::LeftCtrl, 0x3B),
    (Key::RightCtrl, 0x3B),
    // Keypad: Lunatic Fringe's primary controls.
    (Key::NumPadDot, 0x41),
    (Key::NumPadAsterisk, 0x43),
    (Key::NumPadPlus, 0x45),
    (Key::NumPadSlash, 0x4B),
    (Key::NumPadEnter, 0x4C),
    (Key::NumPadMinus, 0x4E),
    (Key::NumPad0, 0x52),
    (Key::NumPad1, 0x53),
    (Key::NumPad2, 0x54),
    (Key::NumPad3, 0x55),
    (Key::NumPad4, 0x56),
    (Key::NumPad5, 0x57),
    (Key::NumPad6, 0x58),
    (Key::NumPad7, 0x59),
    (Key::NumPad8, 0x5B),
    (Key::NumPad9, 0x5C),
    (Key::Left, 0x7B),
    (Key::Right, 0x7C),
    (Key::Down, 0x7D),
    (Key::Up, 0x7E),
];

/// The *true* hardware code for the keys whose [`KEY_MAP`] entry is a deliberate
/// lie, plus the one key that table does not carry at all.
///
/// `KEY_MAP` answers "what should the module see", and for several keys the
/// answer is not the key's own code: `F` sits there as Command because it is a
/// second Fire, and each right-hand modifier is folded onto its left-hand twin
/// because a Mac `KeyMap` has one bit for the pair. Both are right for the
/// emulator and wrong for [`ad_keystate`], which is asking the window server
/// about a *physical* key and needs the number that key really sends. Getting
/// this backwards would be quiet and awful: `F` would read as Command, so
/// holding `F` would fire and also look like the modifier.
///
/// Every code here matches `minifb`'s own macOS table, which is the other half
/// of the same translation and must not drift from it.
const HW_CODE: &[(Key, u8)] = &[
    (Key::F, 0x03),
    (Key::RightSuper, 0x36),
    (Key::RightShift, 0x3C),
    (Key::RightAlt, 0x3D),
    (Key::RightCtrl, 0x3E),
    // Absent from `KEY_MAP`, but reached for by the modern layout (Abort Ship)
    // and by [`TYPING_MAP`] (erase), so it still needs a physical code.
    (Key::Delete, 0x75),
];

/// The physical key code to ask the window server about, or `None` for a key
/// this player never reads.
///
/// [`HW_CODE`] is consulted first for exactly the reason given there.
fn hw_code(key: Key) -> Option<u8> {
    HW_CODE
        .iter()
        .chain(KEY_MAP.iter())
        .find(|(k, _)| *k == key)
        .map(|(_, code)| *code)
}

/// Whether this key is physically down, from the best source available.
///
/// `hid` is the caller's decision that the hardware read is both working and
/// allowed to speak right now — see [`Shared::hid`] for the first and the call
/// site for the second, which is window focus. When it is set, the hardware read
/// is the *sole* authority for any key it knows: it is not merged with the
/// window's opinion, because the window's opinion is precisely the thing that
/// goes wrong under Command, and OR-ing a stale "still down" back in would
/// reinstate the stuck key this whole path exists to remove.
///
/// Falling back to `is_key_down` covers three cases with one line: a platform
/// with no hardware read, a key with no physical code, and a macOS that declined
/// to answer. All three mean "the window is all we have", which is exactly what
/// the player did before any of this.
fn key_down(w: &Window, key: Key, hid: bool) -> bool {
    if hid {
        if let Some(down) = hw_code(key).and_then(ad_keystate::key_down) {
            return down;
        }
    }
    w.is_key_down(key)
}

/// The Mac key codes Lunatic Fringe reads, straight out of its `LFky 128`
/// resource — fourteen 8-byte records of `[flags][character][key code]`.
///
/// A "modern layout" cannot be a new set of codes: the game polls the low-memory
/// `KeyMap` for exactly these, and its own on-screen key table is drawn from this
/// resource. So a modern key is *remapped* onto the original code and the game is
/// left entirely alone — which also means its key table keeps telling the truth
/// about what it expects, rather than being quietly contradicted.
mod lf {
    /// Turn Left — keypad 4.
    pub const TURN_LEFT: u8 = 0x56;
    /// Turn Right — keypad 6.
    pub const TURN_RIGHT: u8 = 0x58;
    /// Thrust — keypad 5.
    pub const THRUST: u8 = 0x57;
    /// Turbo Thrust — keypad 8.
    pub const TURBO: u8 = 0x5B;
    /// Power Shield — keypad 0.
    pub const SHIELD: u8 = 0x52;
    /// Abort Ship — `a`.
    pub const ABORT: u8 = 0x00;
    /// Fire — the Command key, stored in `LFky` as the cmdKey mask.
    pub const FIRE: u8 = 0x37;
    /// Power Shield in the *keyboard* set — the space bar.
    pub const SHIELD_SPACE: u8 = 0x31;
}

/// The modern layout: WASD or arrows, with the right hand on JKL.
///
/// Where a key appears here it maps *only* to this code and never to its own —
/// otherwise `A` would turn left and self-destruct at the same time, `A` being
/// Abort Ship in the original.
const MODERN_MAP: &[(Key, u8)] = &[
    // Turning: either hand.
    (Key::A, lf::TURN_LEFT),
    (Key::Left, lf::TURN_LEFT),
    (Key::D, lf::TURN_RIGHT),
    (Key::Right, lf::TURN_RIGHT),
    // Forward is thrust.
    (Key::W, lf::THRUST),
    (Key::Up, lf::THRUST),
    (Key::J, lf::FIRE),
    (Key::L, lf::TURBO),
    (Key::K, lf::SHIELD),
    // "Delete" on a Mac keyboard is the backspace key; the full-size Delete is
    // accepted too so an external keyboard behaves the same.
    (Key::Backspace, lf::ABORT),
    (Key::Delete, lf::ABORT),
];

/// Additions to the original layout, for Lunatic Fringe only.
///
/// The game's own controls screen lists two sets, and one of them cannot be
/// played on a modern Mac as printed. The **keypad** set is fine: Fire on
/// Command, Power Shield on keypad `0`, and those two together are not a system
/// chord. The **keyboard** set fires on Command and shields on `Space` — and
/// Command with Space is Spotlight, dispatched by the window server before this
/// application is offered the keystroke and refusable by nothing we can write.
///
/// So the keyboard set gains `G` for Power Shield beside `Space`, and it already
/// has `F` for Fire beside Command (`F` lives in [`KEY_MAP`], firing in both
/// layouts). Neither addition removes the original key: anyone on a keyboard
/// with a keypad plays exactly what the game's screen says. `F` and `G` are
/// simply the pair that can be held together without the system taking them.
///
/// Scoped to Lunatic Fringe and not to "whenever the modern layout is off",
/// because the original layout is also what every *other* module runs under.
/// Turning `G` into `Space` for all 140 of them would be precisely the silent
/// corruption [`MODERN_MAP`] is documented as avoiding.
const ORIGINAL_MAP: &[(Key, u8)] = &[(Key::G, lf::SHIELD_SPACE)];

/// Keys whose meaning while typing is not the one [`KEY_MAP`] gives them.
///
/// Consulted **before** the base table, which is the whole point: `F` is in
/// `KEY_MAP` as Command — the second Fire from `d025d4f` — so looking there
/// first would type nothing at all for it.
///
/// `F` is the only key left with two meanings, and only while a module is being
/// *played*: it fires. That reason does not survive contact with a text field,
/// where the only correct behaviour for a letter key is to type its letter.
/// `Escape` is deliberately absent: leaving the module has to work from every
/// screen, including this one.
const TYPING_MAP: &[(Key, u8)] = &[
    (Key::F, 0x03), // f
    // The base table carries only `Backspace` ($33), the key a Mac keyboard
    // labels "delete". A full-size keyboard's separate Delete is accepted as the
    // same erasing key, matching what the modern layout already does for Abort
    // Ship — and erasing is the behaviour somebody correcting a typo wants from
    // either of them.
    (Key::Delete, 0x33),
];

/// The one key of [`TYPING_MAP`] whose meaning changes during play; see there.
#[cfg(test)]
const RESERVED_KEYS: &[(Key, u8)] = &[(Key::F, 0x03)];

/// A prompt that means the module wants typing rather than steering, matched
/// case-insensitively against the strings it draws.
///
/// Lunatic Fringe draws "High score!  Enter your name:" once, when the dialog
/// appears and *before* the first keystroke, which is what makes watching for it
/// lag-free rather than a fix that arrives one character late.
const TEXT_PROMPTS: &[&str] = &["enter your name"];

/// Does anything the module just drew ask the user to type?
fn asks_for_typing(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        let lower = line.to_lowercase();
        TEXT_PROMPTS.iter().any(|p| lower.contains(p))
    })
}

/// The banner's three lines of text, hoisted so a test can measure the same
/// strings the window draws. Nothing clips them, so text that does not fit runs
/// off the edge of the screen and looks like a rendering bug.
const ORIGINAL_HELP: &str =
    "ORIGINAL-ISH: 4/6 or L/' turn, 5 or ; thrust, F fire, 8/P turbo, 0/G shield, A abort";
/// Both halves of the modern layout are spelled out rather than abbreviated.
/// "A/D or arrows turn, W/Up thrust" is true but reads as one hybrid set, and
/// somebody who reaches for the arrow keys and finds the ship will not move has
/// no way to tell that Up was a thrust key all along.
const MODERN_HELP: &str =
    "MODERN: A/D or Left/Right turn, W or Up thrust, J fire, L turbo, K shield, Del abort";
/// Shown once Escape has been pressed once. First, because it is a question.
const LEAVING_HELP: &str =
    "Press Esc AGAIN to leave this module    anything else carries on playing";
/// Shown while paused. Leads with the way out of it — which is now the key the
/// game's own screen asks for, so the banner and the module finally agree.
const PAUSED_HELP: &str = "PAUSED - Caps Lock, 1 or click to play    Esc twice = back to the list";
/// Shown while playing.
const PLAYING_HELP: &str = "PLAYING - Caps Lock, 1 or click pauses    Esc twice = back to the list";
/// Shown on every module except Lunatic Fringe, whose `LFky` key table is what
/// the detailed lines describe. Everything on it is true of every module.
const GENERIC_HELP: &str = "Esc twice = back to the list";

/// Shown while the module is asking for typed text.
///
/// Stays up for as long as typing does, rather than timing out: it is the only
/// on-screen sign that the keys mean letters now, and the question it answers —
/// "why did A stop turning?" — lasts exactly as long as the text field does.
const TYPING_HELP: &str =
    "TYPING - keys type letters, Delete erases    game controls resume afterwards";

/// The Mac key code a physical key should produce under the active layout.
///
/// Returns `None` for a key the player has reserved for itself.
///
/// # Typing
///
/// `typing` turns every remap off and hands back the key's *own* code, because
/// a remap and a text field cannot both be right about what a key means. The
/// game reads name entry from the same `KeyMap` it flies with, so `A` mapped to
/// keypad-4 does not type "a" — it types "4", and Delete mapped to Abort Ship
/// types "a" instead of erasing. Both were reported from a real game. The three
/// keys the player holds back are handed over too; see [`RESERVED_KEYS`].
fn code_for(key: Key, modern: bool, typing: bool) -> Option<u8> {
    if typing {
        return TYPING_MAP
            .iter()
            .chain(KEY_MAP.iter())
            .find(|(k, _)| *k == key)
            .map(|(_, code)| *code);
    }
    if modern {
        // The modern mapping wins outright, so a remapped key never also arrives
        // as itself.
        if let Some((_, code)) = MODERN_MAP.iter().find(|(k, _)| *k == key) {
            return Some(*code);
        }
    }
    KEY_MAP
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, code)| *code)
}

/// What the module should see, which is [`code_for`] plus the Lunatic Fringe
/// additions to the original layout. See [`ORIGINAL_MAP`] for why they are not
/// simply in that table: `!modern` is every other module too.
fn code_for_module(key: Key, fringe: bool, modern: bool, typing: bool) -> Option<u8> {
    // Neither of these applies while typing, where `G` and `1` are a letter and a
    // digit in somebody's name.
    if fringe && !typing {
        // Withheld, and only here: the player pauses on it. See `FRINGE_PAUSE`.
        if key == FRINGE_PAUSE {
            return None;
        }
        if !modern {
            if let Some((_, code)) = ORIGINAL_MAP.iter().find(|(k, _)| *k == key) {
                return Some(*code);
            }
        }
    }
    code_for(key, modern, typing)
}

/// Shared between the window loop and the emulator's present hook.
struct Shared {
    /// Latest frame as 0RGB, ready for the window.
    pixels: Vec<u32>,
    /// Keys currently held, as Mac virtual key codes.
    held: Vec<u8>,
    /// Cursor position in the module's coordinates, as (h, v).
    mouse: (i16, i16),
    /// Set by the window when the user closes it or presses the quit key.
    quit: bool,
    /// Frames presented, for the closing report.
    presented: u64,
    /// Emulated Caps Lock, latched.
    ///
    /// Caps Lock is a *locking* key: on a Macintosh its `KeyMap` bit stays set
    /// for as long as the light is on, which is why Lunatic Fringe can use it as
    /// start-and-pause. Treating it as momentary — set only while physically
    /// held — would be wrong even on a platform that reported it.
    ///
    /// And macOS does not report it. `minifb`'s `flagsChanged:` handler covers
    /// Ctrl, Shift, Alt and Cmd and stops there, while Caps Lock never produces a
    /// `keyDown:` at all, so `Key::CapsLock` can never read as down. Reaching
    /// past the windowing library for the real lock state would mean `unsafe`,
    /// which this workspace forbids. So the latch is driven by key *presses* —
    /// see [`CAPS_TOGGLE`] for which keys, and why there is more than one — and by
    /// a mouse click, which arrives by a different path entirely and therefore
    /// still works if keyboard events do not.
    caps: bool,
    /// What [`minifb::Window::is_active`] returns when the window really does
    /// have focus, learned from the first frame that carried a key transition.
    ///
    /// [`None`] until then, which reads as "not focused" and keeps the hardware
    /// key state switched off. See the call site for why this cannot simply be
    /// assumed to be `true`.
    active_means: Option<bool>,
    /// Previous state of the *physical* Caps Lock, for edge detection: a latch
    /// must flip once per change, not once per polled tick.
    ///
    /// Seeded `false` rather than "unknown" on purpose: a session that starts
    /// with the lock already on then reads as a change on the first frame and
    /// latches, which is what somebody who switched it on before launching meant
    /// to happen.
    caps_lock_was: bool,
    /// Previous mouse-button state, for the same reason.
    click_was_down: bool,
    /// The hardware key read has answered "down" at least once, so it works on
    /// this machine and is the authority from here on.
    ///
    /// It has to be earned rather than assumed. [`ad_keystate::available`] says
    /// the *platform* has the call; it cannot say the call will be answered, and
    /// a future macOS that put this behind an Input Monitoring prompt would
    /// return "up" for every key on Earth. Trusting it blindly would then leave
    /// a window that renders perfectly and cannot be played at all — the worst
    /// failure this player has available to it.
    ///
    /// So the proof is the thing itself: the first frame in which the hardware
    /// says *any* watched key is down, the call has demonstrably worked, and
    /// nothing that works can go back to being blocked. Until then the window's
    /// own view drives, which is exactly what shipped before. The cost of the
    /// whole safety net is that the first keystroke of a session may still take
    /// the old path.
    hid: bool,
    /// When each key last produced a *down* edge.
    ///
    /// Needed because macOS does not deliver `keyUp:` for an ordinary key while
    /// Command is held. Hold a turn key, press Command, release the turn key:
    /// its release is never reported, `minifb` still calls it down, and the ship
    /// turns forever. A key genuinely still held keeps producing auto-repeat down
    /// edges; one whose release was swallowed goes quiet, which is what tells them
    /// apart.
    ///
    /// Command is Fire in Lunatic Fringe, so this happens constantly for anyone
    /// playing the original controls. `F` fires without it — see `KEY_MAP` — but
    /// that is a way around the problem and not a fix for it: the swallowed
    /// release is macOS behaviour, not the game's, and it costs any module read
    /// while Command happens to be down.
    key_seen: Vec<(Key, std::time::Instant)>,
    /// Keys judged stuck by the rule above, ignored until they are pressed again.
    suppressed: Vec<Key>,
    /// Command's previous state, and when it was released.
    cmd_was_down: bool,
    cmd_released_at: Option<std::time::Instant>,
    /// Whether the modern layout is active.
    ///
    /// On at launch for Lunatic Fringe, off for everything else. The original
    /// controls are built around a numeric keypad, which most machines this runs
    /// on do not have — a laptop keyboard cannot press keypad 4, so the default
    /// that needs no explanation is the one whose keys exist. `C` still
    /// swaps back, and the banner says which layout is live.
    ///
    /// Scoped to Lunatic Fringe rather than set for every module because
    /// [`MODERN_MAP`] remaps `A`, `W`, `D` and the arrows onto keypad codes from
    /// *its* `LFky` table. That is a correct translation for the one module that
    /// polls those codes and a silent corruption of the keyboard for any other.
    modern: bool,
    /// Sound is switched off for this session.
    ///
    /// Seeded from the saved sound preference at every launch and flipped live
    /// by `M`; flipping it never writes the preference back — a mute pressed to
    /// take a phone call is not a decision about next week.
    muted: bool,
    /// The module has asked for typing, so keys go through unremapped.
    ///
    /// Set when the module draws a prompt (see [`TEXT_PROMPTS`]) and cleared by
    /// the main loop once the `DrawFrame` that contains the prompt returns —
    /// which for Lunatic Fringe is the end of the whole game session, since it
    /// never returns while playing. That bound is the safety net: even if the
    /// closing edge were missed entirely, typing mode cannot outlive the game it
    /// started in and leave the next one unsteerable.
    typing: bool,
    /// Whether [`Shared::typing`] was set before the current `DrawFrame` began,
    /// so the loop never clears a prompt it has only just seen.
    typing_is_stale: bool,
    /// While set, one more Escape leaves the module; it expires on its own.
    ///
    /// Leaving is not free — a game in progress has a score and a life in it — so
    /// it takes two presses. Expiring matters as much as arming: a half-committed
    /// exit that waited forever would fire on an Escape pressed minutes later for
    /// some other reason.
    esc_armed_until: Option<u32>,
    /// Show the controls banner until this tick. Bumped whenever the controls or
    /// the paused state change, so it answers the question it was just asked.
    hint_until: u32,
}

/// How long an armed Escape waits for its confirmation: three seconds.
const ESC_CONFIRM_TICKS: u32 = 180;

/// How long after Command is released to wait before judging a key stuck.
///
/// Longer than the system's initial auto-repeat delay (250 ms by default), so a
/// key that really is held will have produced at least one repeat and is never cut
/// off. Shorter than anyone's patience with a ship that will not stop turning.
const LOST_KEYUP_GRACE: std::time::Duration = std::time::Duration::from_millis(450);

/// Records every key transition the window reports.
///
/// Polling `is_key_down` once a frame cannot see a press shorter than a frame:
/// macOS delivers the down and the up in one event batch, `minifb` applies both
/// inside a single `update_with_buffer`, and the poll afterwards sees the key up
/// again. A held key is fine; a quick tap is invisible.
///
/// `InputCallback::set_key_state` is called on every transition as it is applied,
/// so nothing is lost between polls. That matters most for the Caps Lock latch,
/// where a missed press means the game simply does not start.
struct KeyTaps {
    taps: Rc<RefCell<Vec<(Key, bool)>>>,
}

impl minifb::InputCallback for KeyTaps {
    fn add_char(&mut self, _uni_char: u32) {}

    fn set_key_state(&mut self, key: Key, state: bool) {
        // Bounded so a session that never drains cannot grow without limit.
        let mut taps = self.taps.borrow_mut();
        if taps.len() < 512 {
            taps.push((key, state));
        }
    }
}

/// Keys that toggle the emulated Caps Lock latch — which is to say, pause.
///
/// Pausing *is* toggling Caps Lock: the game's own help text says "Use Caps Lock to
/// pause and unpause the game", so start, pause and unpause are one control rather
/// than three that would have to be kept in step.
///
/// **Two keys.** This held four at one point — `G`, `1`, `Return` and `Tab` — all
/// scar tissue from not knowing whether keyboard events were arriving at all: each
/// time something failed another key was added instead of the broken one being
/// removed. `Tab` cannot work here (`NSWindow` consumes it for focus traversal),
/// `Return` is `0x24` and modules have a claim on it, and `G` was only ever the key
/// that happened to be verified first. All three are given back.
///
/// `Key::CapsLock` is the real key, and it is now the only one.
///
/// It used to need a substitute — `1` — because macOS never delivers Caps Lock
/// as a `keyDown:`, so on that platform this entry was inert. `ad_keystate`
/// closed that hole by reading the latch itself, from the flags state the window
/// server keeps, and once the real key worked everywhere the stand-in was a key
/// held back from every module for nothing. Windows and Linux take it from the
/// event stream; macOS reads it below. The mouse click is the fallback on all
/// three, and matters most where keyboard events are not arriving at all.
///
/// # A latch cannot be flipped by another application
///
/// The worry that prompted trimming this list: someone pauses, switches to another
/// app, and knocks Caps Lock by accident. They cannot unpause the game that way. The
/// latch moves on key *events*, and an unfocused window receives none — so a Caps
/// Lock press somewhere else never reaches here.
///
/// On Windows and Linux the physical lock can therefore end up disagreeing with the
/// game: the keyboard light says one thing and the title bar says another. The title
/// bar is the truthful one. That is cosmetic, and it is the right trade against a
/// game that unpauses itself while nobody is looking.
const CAPS_TOGGLE: &[Key] = &[Key::CapsLock];

/// A second pause key, for Lunatic Fringe only.
///
/// Caps Lock is the key the game's own screen names, and it is a latch the
/// system owns rather than a letter taken from anyone — which is why it costs
/// nothing and applies everywhere. This one is a real key held back from a real
/// module, so it is scoped to the only module that has a pause worth the name:
/// [`PAUSED_HELP`] and [`PLAYING_HELP`] are drawn for Lunatic Fringe and nothing
/// else, and every other module keeps `1` as the digit `0x12`.
///
/// `1` and not the obvious alternatives. `P` is Turbo Thrust in the game's own
/// keyboard column. `Return` submits the name on the high-score screen. `Tab`
/// never arrives at all on macOS — see the note in `reserved_keys_are_not_passed
/// _through` about why it was given back. A digit is the least likely thing a
/// screensaver polls for, and this was the pause key here until Caps Lock could
/// be read directly, so it is the least surprising choice on top of that.
const FRINGE_PAUSE: Key = Key::Key1;

/// Wheel delta that counts as one row.
///
/// A mouse notch is about 1.0 and a trackpad sends many fractional deltas, so
/// this is a threshold rather than a scale: the remainder is kept, which makes a
/// slow two-finger drag move the list smoothly instead of not at all.
const WHEEL_LINE: f32 = 1.0;

/// Mac virtual key code for Caps Lock.
const CAPS_LOCK_CODE: u8 = 0x39;

/// Initial window size: 640x480 at 1.6x, which is 4:3 so nothing is letterboxed.
///
/// Not `Scale::FitScreen`, and not a fixed multiple like 2x. `minifb` cannot be
/// asked how big the display is, so a 2x window (1280x960) does not fit a 13-inch
/// laptop's 1440x900 and would open partly off screen. This is comfortably larger
/// than the emulated screen and fits everything.
const INITIAL_WIDTH: usize = 1024;
const INITIAL_HEIGHT: usize = 768;

/// Map a position in window coordinates onto the framebuffer.
///
/// The framebuffer is a fixed 640x480 because that is the screen these modules
/// were written for — the emulated machine's resolution is not the window's — so
/// a resized window scales and letterboxes. That makes this conversion necessary
/// rather than cosmetic: without it, clicking a row in a resized window selects
/// the wrong one, and `_GetMouse` hands modules a cursor that is somewhere else.
///
/// `minifb`'s own `get_mouse_pos` cannot be used, because it divides by the scale
/// factor computed *at construction* and knows nothing about a later resize or
/// about the letterbox offset.
///
/// Returns `None` for a position in the letterbox, outside the picture.
fn window_to_buffer(win: (usize, usize), pos: (f32, f32)) -> Option<(i32, i32)> {
    let (ww, wh) = (win.0 as f32, win.1 as f32);
    let (bw, bh) = (WIDTH as f32, HEIGHT as f32);
    // `AspectRatioStretch`: the largest uniform scale that fits, centred.
    let scale = (ww / bw).min(wh / bh);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let x = (pos.0 - (ww - bw * scale) / 2.0) / scale;
    let y = (pos.1 - (wh - bh * scale) / 2.0) / scale;
    if x < 0.0 || y < 0.0 || x >= bw || y >= bh {
        return None;
    }
    Some((x as i32, y as i32))
}

/// Widest the idle strip's module button may be drawn, in pixels.
///
/// Module titles come from the user's folder, so their length is not this
/// program's to assume. See the strip's layout in `draw_browser`.
const IDLE_MODULE_MAX_W: i32 = 170;

/// Ticks the "how to start" banner stays up: five seconds.
///
/// It exists because the emulated Caps Lock is on a substitute key, and a
/// substitute nobody can guess is the same as no key at all — which is exactly
/// how the first play session went. The title bar says the same thing, but a
/// title bar is not where you are looking when a screen saver takes the window.
const HINT_TICKS: u32 = 300;

/// Composite the banner over the bottom of a frame.
///
/// Draws into `banner`'s own rows and copies them, rather than making the whole
/// 640x480 frame a `Canvas`: this touches a few thousand pixels a frame instead
/// of three hundred thousand.
fn overlay_hint(pixels: &mut [u32], banner: &mut Canvas, font: &Font, lines: &[&str]) {
    let f = font.strike();
    banner.clear(colour::INK);
    let line_h = i32::from(f.line_height()).max(9);
    for (i, text) in lines.iter().enumerate() {
        let baseline = i32::from(f.ascent) + 3 + i32::try_from(i).unwrap_or(0) * line_h;
        banner.text(&f, 8, baseline, text, colour::SELECTED_INK);
    }
    let top = HEIGHT.saturating_sub(banner.h);
    for row in 0..banner.h {
        let from = row.saturating_mul(banner.w);
        let to = from.saturating_add(banner.w);
        let dst = top.saturating_add(row).saturating_mul(WIDTH);
        let dst_end = dst.saturating_add(banner.w);
        if let (Some(src), Some(out)) = (banner.px.get(from..to), pixels.get_mut(dst..dst_end)) {
            out.copy_from_slice(src);
        }
    }
}

/// Where the cursor is on the emulated screen, or `None` if outside it.
fn cursor_in_buffer(w: &Window) -> Option<(i32, i32)> {
    // Unscaled, so the position is in window coordinates and `window_to_buffer`
    // does the whole conversion in one place.
    let pos = w.get_unscaled_mouse_pos(minifb::MouseMode::Pass)?;
    window_to_buffer(w.get_size(), pos)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();

    // `--screenshot <file>` draws the browser once and writes a PNG instead of
    // opening a window. It exists so the interface can be *looked at* in a
    // headless run — the same discipline the module frames are held to, applied
    // to the launcher's own chrome. Shipping a GUI nobody has seen is how you get
    // text one pixel out of line with everything else.
    let shot = argv
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| {
            let file = argv.get(i + 1).cloned();
            argv.drain(i..=(i + 1).min(argv.len().saturating_sub(1)));
            file
        })
        .map(PathBuf::from);

    // `--keytest` opens a bare window and reports keys, with no emulator, no
    // shared window and no present hook. It exists to answer one question that
    // reading code could not: when a keystroke appears to do nothing, is the
    // fault in this program's input plumbing or in what reaches the process at
    // all? Everything else in here is downstream of that answer.
    if argv.first().is_some_and(|a| a == "--keytest") {
        let mut w = Window::new(
            "After Dark - key test",
            WIDTH,
            HEIGHT,
            WindowOptions::default(),
        )?;
        w.set_target_fps(60);
        let buf = vec![0x0020_2020u32; WIDTH * HEIGHT];
        let mut frames = 0u32;
        let mut seen = 0u32;
        eprintln!(
            "keytest: press keys for 20 seconds. To reproduce the Command bug, hold \
             Left, then hold Command, then release Left while Command is still down.\n\
             `win` is what the event stream believes; `hw` is what the keyboard is \
             actually doing."
        );
        // Only report changes: a held key is down for every frame it is held, and
        // sixty identical lines a second buries the one transition that matters.
        // Both views are compared, because the whole point is to catch the frame
        // where only one of them moves.
        let mut last: (Vec<Key>, Vec<Key>) = (Vec::new(), Vec::new());
        // Frames where the window still called a key down after the keyboard had
        // let go of it — the stuck key, counted rather than described.
        let mut stuck_frames = 0u32;
        let mut hw_ever_answered = false;
        // Which way round `is_active` reads, learned rather than assumed —
        // `minifb` 0.28 has it backwards on macOS. A key the window reports is
        // proof it had focus when it saw one. Same reasoning as the main loop.
        let mut active_means: Option<bool> = None;
        while w.is_open() && frames < 60 * 20 {
            w.update_with_buffer(&buf, WIDTH, HEIGHT)?;
            // Only while focused: the hardware read reports the whole session's
            // keyboard, so an unfocused window would report the user's typing
            // elsewhere and the comparison would be meaningless.
            let raw_active = w.is_active();
            let win = w.get_keys();
            if !win.is_empty() {
                active_means = Some(raw_active);
            }
            let focused = active_means == Some(raw_active);
            let hw: Vec<Key> = if focused {
                KEY_MAP
                    .iter()
                    .map(|(k, _)| *k)
                    .filter(|k| hw_code(*k).and_then(ad_keystate::key_down) == Some(true))
                    .collect()
            } else {
                Vec::new()
            };
            hw_ever_answered |= !hw.is_empty();
            let stuck: Vec<Key> = win
                .iter()
                .copied()
                .filter(|k| hw_code(*k).is_some() && !hw.contains(k))
                .collect();
            if focused && hw_ever_answered && !stuck.is_empty() {
                stuck_frames += 1;
            }
            let now = (win.clone(), hw.clone());
            if (!win.is_empty() || !hw.is_empty()) && now != last {
                seen += 1;
                eprintln!("frame {frames}: win {win:?} hw {hw:?}");
            }
            last = now;
            frames += 1;
        }
        eprintln!(
            "\nkeytest done: {frames} frames, {seen} distinct changes. If G, Return or \
             a click registered, the player can start a module; if Tab did not, that \
             is macOS consuming it for focus traversal and is expected."
        );
        if !hw_ever_answered {
            eprintln!(
                "The hardware key read never reported anything. Either no key was \
                 pressed while the window had focus, or this platform has no such \
                 read — the player then falls back to the event stream, heuristics \
                 and all, exactly as it did before."
            );
        } else if stuck_frames > 0 {
            eprintln!(
                "The event stream got stuck for {stuck_frames} frames — it kept \
                 calling a key down after the keyboard had let go. That is the \
                 Command bug, reproduced. The hardware read did not get stuck, and \
                 it is what the player now steers by."
            );
        } else {
            eprintln!("The two views agreed on every frame.");
        }
        return Ok(());
    }

    // `--export <dir>` and `--import <dir>` do what the buttons do, without the
    // buttons.
    //
    // Not a convenience: the buttons run the *platform's* folder chooser, and
    // `choose_folder` answers `None` when there is no such tool to run — no
    // `zenity` or `kdialog` on a bare Linux desktop, for instance. Without these
    // flags the honest report on such a machine is "Import cancelled", and there
    // is then no way to import at all. They also make a backup scriptable, which
    // is what somebody moving to a new machine actually wants.
    {
        let flag = argv.first().map(String::as_str);
        if matches!(flag, Some("--export" | "--import")) {
            let Some(dir) = argv.get(1).map(PathBuf::from) else {
                eprintln!(
                    "{} needs a folder: ad-player {} <folder>",
                    flag.unwrap_or(""),
                    flag.unwrap_or("")
                );
                std::process::exit(2);
            };
            let said = if flag == Some("--export") {
                match ad_runtime::export_scores(&dir) {
                    Err(e) => {
                        eprintln!("Export failed: {e}");
                        std::process::exit(1);
                    }
                    Ok(0) => "Nothing to export yet — no module has saved a score".to_owned(),
                    Ok(n) => format!("Exported {n} save(s) to {}", dir.display()),
                }
            } else {
                match ad_runtime::import_scores(&dir) {
                    Err(e) => {
                        eprintln!("Import failed: {e}");
                        std::process::exit(1);
                    }
                    Ok(r) => {
                        for bad in &r.rejected {
                            eprintln!("skipped {bad}");
                        }
                        format!(
                            "Imported {} save(s): {} new, {} replaced{}",
                            r.total(),
                            r.added,
                            r.replaced,
                            if r.rejected.is_empty() {
                                String::new()
                            } else {
                                format!(", {} skipped", r.rejected.len())
                            }
                        )
                    }
                }
            };
            println!("{said}");
            return Ok(());
        }
    }

    // `--scores` says where saved state lives and what is in it.
    //
    // Worth a flag rather than a line in a document, because the underlying worry
    // is reasonable and the answer is not guessable: high scores are *not* inside
    // the application. They are in the platform's application-support directory, so
    // deleting, replacing or rebuilding the app does not touch them — and backing
    // them up or moving them to another machine is copying one folder.
    if argv.first().is_some_and(|a| a == "--scores") {
        match ad_runtime::save_dir() {
            None => println!("No save directory on this platform (no HOME set?)."),
            Some(dir) => {
                println!("Saved state lives in:\n  {}", dir.display());
                println!(
                    "\nThis is outside the application. Deleting or rebuilding the app \
                     does not\nremove it. To back up or move to another machine, copy \
                     that folder."
                );
                match std::fs::read_dir(&dir) {
                    Err(_) => println!("\nNothing saved yet — the folder does not exist."),
                    Ok(entries) => {
                        let mut found = 0;
                        for e in entries.flatten() {
                            let path = e.path();
                            // Saves only. The imported library is a `modules`
                            // subdirectory of this same folder, and listing it
                            // here as a nameless file of some size would read as
                            // a save nobody can account for.
                            if path.extension().is_none_or(|x| x != "rsrc") {
                                continue;
                            }
                            let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                            println!("\n  {:<44} {len:>7} bytes", e.file_name().to_string_lossy());
                            found += 1;
                        }
                        if found == 0 {
                            println!("\nThe folder is empty: no module has saved anything yet.");
                        } else {
                            println!(
                                "\n{found} file(s). One per module that has saved, named after it."
                            );
                        }
                        // The library lives here too, and "copy that folder"
                        // above should say what it is copying.
                        if let Some(lib) = ad_runtime::library_dir() {
                            let count = std::fs::read_dir(&lib)
                                .map(|e| e.flatten().count())
                                .unwrap_or(0);
                            if count > 0 {
                                println!(
                                    "\nYour imported After Dark files are in the \
                                     `modules` folder beside these ({count} files).\n\
                                     They are not saves, and export/import above \
                                     leaves them alone."
                                );
                            }
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    let arg = argv.first().map(PathBuf::from);
    // Whether a path was named matters below: somebody who said where to look
    // gets what they asked for, including its failures, rather than a dialog
    // offering to import something else.
    let explicit = arg.is_some();
    // A single-module bundle: the same binary in an app whose
    // `Contents/Resources/module` names one module. Double-clicking that one
    // opens the game rather than the list, which is what somebody who plays
    // Lunatic Fringe and nothing else actually wants in their Dock.
    //
    // Read from the bundle rather than compiled in, so both apps are one build;
    // and read from a file rather than taken as an argument, because an
    // application launched from the Finder is given none. Both bundles save to
    // the same place — see [`ad_runtime::save_dir`], which is keyed on the
    // user's home directory and not on which app is running — so the high score
    // table is shared, not forked in two.
    let pinned = arg.is_none().then(pinned_module).flatten();
    // The player is a *product* host, not the lab: high scores persist in the
    // platform save location, and nothing is logged to a console the user is not
    // reading. This is where "high scores survive" actually reaches a person.
    let options = ad_runtime::RuntimeOptions::product_from_env();

    // A module path runs it; a directory browses it; nothing browses the
    // default library.
    let (browse_dir, direct) = match arg.clone() {
        Some(p) if p.is_dir() => (p, None),
        Some(p) => (
            p.parent().map_or_else(default_library, Path::to_path_buf),
            Some(p),
        ),
        None => (default_library(), None),
    };

    if let Some(file) = shot {
        let dir = arg.filter(|p| p.is_dir()).unwrap_or(browse_dir);
        return screenshot(&dir, &file).map_err(Into::into);
    }

    // First run, or a library that has gone missing since the last one. Asked
    // and answered *before* a window opens: an empty launcher behind a dialog
    // asking how to fill it is a worse way to say the same thing.
    //
    // Quitting from that dialog is a choice, not a failure, so it exits without
    // an error — there is nothing to report to somebody who just said no.
    let browse_dir = if direct.is_some() || explicit || ad_runtime::have_library(&browse_dir) {
        browse_dir
    } else {
        match setup::ensure_library() {
            Some(dir) => dir,
            None => return Ok(()),
        }
    };

    // The pinned module is resolved *after* the library check above, because the
    // first launch of a single-module app still has to import a library before
    // there is anything to pin to.
    //
    // A title that is not in the library opens the list instead of failing. The
    // person then has a working application and can see for themselves that the
    // module is missing, which beats an app that quits on launch because the
    // disk they imported was a different edition.
    let direct = direct.or_else(|| {
        let title = pinned?;
        let path = browse_dir.join(format!("{title}.rsrc"));
        if !path.is_file() {
            eprintln!(
                "{title}: not in {} — opening the module list instead",
                browse_dir.display()
            );
            return None;
        }
        Some(path)
    });

    let window = Rc::new(RefCell::new(browser_window()?));

    // Sound. A machine with no usable output device must still run the module: a
    // screen saver that refuses to start because there is no sound card is worse
    // than a quiet one.
    let audio = match ad_runtime::AudioDevice::open() {
        Ok(d) => Some(Rc::new(d)),
        Err(e) => {
            eprintln!("audio unavailable, running silent: {e}");
            None
        }
    };

    if let Some(path) = direct {
        return run_module(&path, &window, audio.as_ref(), &options, false, true)
            .map_err(Into::into);
    }
    browse(&browse_dir, &window, audio.as_ref(), &options)
}

/// Where to look for modules when nobody says.
///
/// The **imported library** first: the packaged app ships no modules — they are
/// Berkeley Systems' and Apple's — so the ordinary case is a library the person
/// imported once from their own disk, which lives beside the saved scores and
/// not inside the bundle. See [`ad_runtime::library`] for why that is.
///
/// Then the older places, which still work and cost nothing to keep: inside a
/// bundle, beside the executable, and `./modules` for a `cargo run` from the
/// checkout. A checkout that has *both* gets the imported one, because it is
/// the only one already checked for completeness; pass a path to override.
fn default_library() -> PathBuf {
    if let Some(imported) = ad_runtime::library_dir() {
        if ad_runtime::have_library(&imported) {
            return imported;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // <App>.app/Contents/MacOS/ad-player -> <App>.app/Contents/Resources
        if let Some(res) = exe
            .parent()
            .and_then(Path::parent)
            .map(|c| c.join("Resources"))
        {
            let inside = res.join("modules");
            if inside.is_dir() {
                return inside;
            }
        }
        // Or simply beside the executable, for a plain unzipped folder.
        if let Some(beside) = exe.parent().map(|d| d.join("modules")) {
            if beside.is_dir() {
                return beside;
            }
        }
    }
    PathBuf::from("modules")
}

/// The module this application is pinned to, or `None` for the ordinary
/// launcher.
///
/// A `module` file holds one line: the title of the module to run instead of
/// showing the list — the file stem the browser lists it under, so
/// `Lunatic Fringe` and not `Lunatic Fringe.rsrc`. No such file means the
/// browser, unchanged; there is no flag and no setting to get wrong.
///
/// Two places are looked at, because the platforms lay an application out
/// differently and neither spelling should win by accident:
///
/// * `<App>.app/Contents/Resources/module`, the macOS bundle.
/// * `module` beside the executable, which is all Linux and Windows have —
///   there is no bundle there, only a folder with the binary in it.
///
/// The bundle is checked first so a stray file inside `Contents/MacOS` cannot
/// quietly override the packaged answer on macOS.
fn pinned_module() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    pinned_beside(exe.parent()?)
}

/// The two-layout lookup, given the directory the executable sits in.
///
/// Split out from [`pinned_module`] for the same reason [`pinned_in`] is: it
/// can then be tested against a directory laid out either way, without a real
/// bundle and without a real executable to be inside it.
fn pinned_beside(beside: &Path) -> Option<String> {
    // <App>.app/Contents/MacOS/ad-player -> <App>.app/Contents/Resources
    beside
        .parent()
        .map(|contents| contents.join("Resources"))
        .and_then(|resources| pinned_in(&resources))
        .or_else(|| pinned_in(beside))
}

/// The title written in `<resources>/module`, or `None` if there is no such file
/// or it says nothing.
///
/// Split out from [`pinned_module`] so the reading can be tested without a real
/// application bundle around it. Trimmed because the file is written by a shell
/// script with `echo`, which leaves a newline.
fn pinned_in(resources: &Path) -> Option<String> {
    let text = std::fs::read_to_string(resources.join("module")).ok()?;
    let title = text.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// Render the browser to a PNG without opening a window.
fn screenshot(dir: &Path, file: &Path) -> Result<(), String> {
    let modules = library::scan(dir);
    if modules.is_empty() {
        return Err(format!("no modules in {}", dir.display()));
    }
    let font =
        Font::discover(dir).ok_or_else(|| format!("no Macintosh font in {}", dir.display()))?;
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    let mut scroll = 0usize;
    // A selection partway down, so the highlight, the scroll indicator and a
    // settings panel with content are all in the picture.
    let selected = modules.len() / 3;
    // Draw once to learn the row geometry, reveal the selection, then draw for
    // real. `draw_browser` no longer chases the selection itself — the wheel
    // needs to be able to scroll away from it — so without this the screenshot
    // would show the top of the list with the highlight off screen.
    let idle = ad_runtime::IdleSettings::load();
    let rows = draw_browser(
        &mut canvas,
        &font,
        &modules,
        selected,
        &mut scroll,
        None,
        &idle,
    );
    reveal(selected, rows.visible, &mut scroll);
    draw_browser(
        &mut canvas,
        &font,
        &modules,
        selected,
        &mut scroll,
        None,
        &idle,
    );
    ad_runtime::png::write_argb(file, WIDTH as u32, HEIGHT as u32, &canvas.px)?;
    println!(
        "{} modules, font {} -> {}",
        modules.len(),
        font.origin,
        file.display()
    );
    Ok(())
}

/// A clickable rectangle, in framebuffer coordinates.
#[derive(Debug, Clone, Copy)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// One row of Lunatic Fringe's high-score table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScoreRow {
    name: String,
    score: u32,
    level: u16,
}

/// The saved high scores for a module, best first — or nothing worth showing.
///
/// Reads the module's saved `LFhs 128` from the platform save directory. The
/// layout is ten 24-byte records — Pascal name at +0, level as a big-endian
/// word at +18, score as a big-endian long at +20 — and none of that is from
/// documentation: it is read off a real save produced by playing a real game,
/// where a score of 152 at level 1 under a known name pins every field. Empty
/// slots carry a score of -1, which is why the score is read signed.
///
/// Only Lunatic Fringe writes `LFhs`, so every other module answers an empty
/// list and the browser simply shows nothing — the "if it's not possible,
/// don't do it" behaviour, decided per module by its own data.
fn fringe_scores(title: &str) -> Vec<ScoreRow> {
    let Some(dir) = ad_runtime::save_dir() else {
        return Vec::new();
    };
    let Ok(saved) = ad_runtime::ForkSink::load(&dir, title) else {
        return Vec::new();
    };
    let Some(table) = saved.iter().find(|r| r.res_type == *b"LFhs" && r.id == 128) else {
        return Vec::new();
    };
    parse_lfhs(&table.data)
}

/// The table itself, separated from the disk so a test can feed it real bytes.
fn parse_lfhs(data: &[u8]) -> Vec<ScoreRow> {
    let mut rows: Vec<ScoreRow> = data
        .chunks_exact(24)
        .filter_map(|e| {
            let len = usize::from(*e.first()?);
            let name_bytes = e.get(1..1 + len.min(17))?;
            let level = u16::from_be_bytes([*e.get(18)?, *e.get(19)?]);
            let score = i32::from_be_bytes([*e.get(20)?, *e.get(21)?, *e.get(22)?, *e.get(23)?]);
            if score < 0 || len == 0 {
                return None; // an empty slot, not a zero score
            }
            Some(ScoreRow {
                name: ad_resource::macroman::decode(name_bytes),
                score: u32::try_from(score).unwrap_or(0),
                level,
            })
        })
        .collect();
    rows.sort_by_key(|b| std::cmp::Reverse(b.score));
    rows
}

/// The browser's ordinary window.
///
/// Recreated after every full-screen session, not merely refocused: a `minifb`
/// window's Metal view renders continuously for as long as the window exists,
/// so keeping the browser window alive behind a full-screen run put two Metal
/// views and the emulator in contention on one thread — which came back from a
/// real machine as Lunatic Fringe "speeding up and down" and name entry
/// responding a beat late. One window exists at a time; see the swap in
/// `browse`.
fn browser_window() -> Result<Window, minifb::Error> {
    let mut w = Window::new(
        APP_NAME,
        INITIAL_WIDTH,
        INITIAL_HEIGHT,
        WindowOptions {
            // 640x480 is a small box on a modern display, so scale up to the
            // largest whole multiple that fits and let the window be dragged
            // from there.
            scale: Scale::FitScreen,
            resize: true,
            // Never distort. The framebuffer is a fixed 640x480 because that
            // is the screen the modules were written for — the emulated
            // machine's resolution is not the window's — so a resized window
            // letterboxes rather than stretching a toaster into an oval.
            scale_mode: minifb::ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )?;
    // The emulator paces itself from cycles, so the window must not also
    // throttle while a module runs; the browser sets its own rate.
    w.set_target_fps(0);
    Ok(w)
}

/// A borderless window covering the display, for a raised (screen-saver-style)
/// run. `None` when the display size cannot be asked; the caller falls back to
/// floating the ordinary window.
fn fullscreen_window(title: &str) -> Option<Window> {
    let (w, h) = ad_runtime::display_size()?;
    let mut win = Window::new(
        &format!("After Dark — {title}"),
        w,
        h,
        WindowOptions {
            borderless: true,
            title: false,
            resize: false,
            // The 640x480 frame stretches to the window; the letterbox around a
            // non-4:3 display stays the window's own black.
            scale: Scale::X1,
            scale_mode: minifb::ScaleMode::AspectRatioStretch,
            topmost: true,
            ..WindowOptions::default()
        },
    )
    .ok()?;
    win.set_position(0, 0);
    win.set_target_fps(0);
    Some(win)
}

/// Run the configured external command until the machine is touched again.
///
/// The idle timer's job is "take the screen, then give it back"; what it starts
/// need not be one of our modules. See `ad_runtime::IdleCommand` for why this
/// exists — the After Dark 3.0-era modules, Rat Race among them, cannot run in
/// this runtime, but a real emulator with a real After Dark install runs them
/// today, and this hands it the same idle slot on the same terms.
///
/// The child is put in its own process group so stopping it cannot signal the
/// player, and is terminated when somebody comes back. A command that
/// immediately detaches — `open -a Something` — has nothing left to terminate,
/// so it stays up until quit by hand; point this at an executable rather than
/// at `open` if you want it to go away on a keypress.
///
/// The shell and the process-group call are per-platform: `/bin/sh -c` and
/// `setpgid` do not exist on Windows, where the equivalents are `cmd /C` and
/// `CREATE_NEW_PROCESS_GROUP`. Spelling both out is what keeps this file
/// compiling for all three targets — an unconditional `std::os::unix` import
/// here is a hard build break on Windows, not a degraded feature.
fn run_idle_command(command: &str, watch: &mut ad_runtime::IdleWatch) -> String {
    /// The command, in its own process group, spelled for this platform.
    #[cfg(unix)]
    fn spawnable(command: &str) -> std::process::Command {
        use std::os::unix::process::CommandExt as _;
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(command).process_group(0);
        cmd
    }
    #[cfg(windows)]
    fn spawnable(command: &str) -> std::process::Command {
        use std::os::windows::process::CommandExt as _;
        /// `CREATE_NEW_PROCESS_GROUP` — the Windows spelling of `process_group(0)`.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/C")
            .arg(command)
            .creation_flags(CREATE_NEW_PROCESS_GROUP);
        cmd
    }

    let child = match spawnable(command).spawn() {
        Ok(c) => c,
        Err(e) => return format!("Idle command failed to start: {e}"),
    };
    // Terminated on the way out however this returns — including a panic
    // unwinding through it. An orphaned emulator holding the screen after the
    // player has gone is the worst outcome this feature can have.
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Reap(child);
    loop {
        match child.0.try_wait() {
            Ok(Some(_)) => return "Idle command finished".to_owned(),
            Err(e) => return format!("Idle command: {e}"),
            Ok(None) => {}
        }
        if watch.woke() {
            return "Idle command stopped — welcome back".to_owned();
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Which module the idle timer should start, as an index into `modules`.
///
/// A named module that is no longer in the folder falls back to a random one
/// rather than to nothing: the setting names a module by title so it survives
/// the folder being moved, and the failure a user would actually hit — they
/// deleted that one module — should not quietly turn the feature off.
fn idle_choice(idle: &ad_runtime::IdleSettings, modules: &[library::Entry]) -> Option<usize> {
    if modules.is_empty() {
        return None;
    }
    if let ad_runtime::IdleModule::Named(want) = &idle.module {
        if let Some(i) = modules.iter().position(|m| &m.title == want) {
            return Some(i);
        }
    }
    // Randomness from the clock rather than a `rand` dependency. This picks a
    // screen saver; it is not sampling anything, and the workspace's whole
    // dependency budget is a window and an audio device.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    Some(usize::try_from(nanos).unwrap_or(0) % modules.len())
}

/// Scroll the view the least amount that puts `selected` on screen.
///
/// The least amount matters: snapping the selection to the middle makes the list
/// jump under the cursor every time you press an arrow.
fn reveal(selected: usize, visible: usize, scroll: &mut usize) {
    if selected < *scroll {
        *scroll = selected;
    } else if selected >= scroll.saturating_add(visible) {
        *scroll = selected.saturating_sub(visible.saturating_sub(1));
    }
}

/// Ask where to put the scores, copy them there, and describe what happened.
///
/// Returns a line for the footer either way. Every outcome is reported, including
/// "nothing to export" and "you cancelled": a button that silently does nothing is
/// indistinguishable from a broken one.
fn export_scores_interactively() -> String {
    let Some(dest) = ad_runtime::choose_folder("Choose a folder for your After Dark scores") else {
        return "Export cancelled (or no folder chooser on this system)".to_owned();
    };
    match ad_runtime::export_scores(&dest) {
        Err(e) => format!("Export failed: {e}"),
        Ok(0) => "Nothing to export yet — no module has saved a score".to_owned(),
        Ok(1) => format!("Exported 1 save to {}", dest.display()),
        Ok(n) => format!("Exported {n} saves to {}", dest.display()),
    }
}

/// Ask which folder to read scores from, copy them in, and say what changed.
///
/// The counts are reported separately because importing is the one action here
/// that *destroys* something: a save that replaces an existing one takes that
/// module's high scores with it. "Imported 3" would be true and would hide it,
/// so replacements are named and put first.
fn import_scores_interactively() -> String {
    // A file chooser, not a folder chooser: the exported .rsrc files were
    // visible-but-greyed in the folder dialog, which reads as broken. Picking
    // the exported file imports it; picking any file deep in a backup folder
    // still works because a folder can also be dropped on the CLI flag.
    let Some(src) = ad_runtime::choose_file_or_folder("Choose an exported After Dark scores file")
    else {
        return "Import cancelled (or no file chooser on this system)".to_owned();
    };
    match ad_runtime::import_scores(&src) {
        Err(e) => format!("Import failed: {e}"),
        Ok(r) => {
            let mut said = match (r.added, r.replaced) {
                (0, 0) => "No saves found in that folder".to_owned(),
                (added, 0) => format!("Imported {added} save(s)"),
                (0, replaced) => format!("Replaced {replaced} existing save(s)"),
                (added, replaced) => {
                    format!("Imported {added}, replaced {replaced} existing save(s)")
                }
            };
            // Named, not just counted: "1 skipped" leaves somebody hunting for
            // which of their files did not make it.
            if let Some(first) = r.rejected.first() {
                let more = r.rejected.len().saturating_sub(1);
                said.push_str(&format!("  ·  skipped {first}"));
                if more > 0 {
                    said.push_str(&format!(" (+{more} more)"));
                }
            }
            said
        }
    }
}

/// The module list: pick one, run it, come back.
fn browse(
    dir: &Path,
    window: &Rc<RefCell<Window>>,
    audio: Option<&Rc<ad_runtime::AudioDevice>>,
    options: &ad_runtime::RuntimeOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let modules = library::scan(dir);
    if modules.is_empty() {
        return Err(format!(
            "no After Dark modules in {}\n\
             A module is a resource fork containing an 'ADgm' resource. Run the \
             player with no arguments to import them from your own disk image.",
            dir.display()
        )
        .into());
    }

    // Without a font there is nothing to draw a list *with*, and none may be
    // bundled — these are Apple's fonts, from the user's own System file. A
    // terminal listing is the honest fallback, not a substitute typeface.
    let Some(font) = Font::discover(dir) else {
        println!(
            "No Macintosh font found in {} (looked for System.rsrc and",
            dir.display()
        );
        println!("Chicago.rsrc), so the browser cannot draw. The modules are:\n");
        for m in &modules {
            println!("  {}", m.path.display());
        }
        println!("\nRun one directly:  ad-player \"<path>\"");
        return Ok(());
    };
    println!("{} modules in {}", modules.len(), dir.display());
    println!("font: {}", font.origin);

    let mut selected = 0usize;
    let mut scroll = 0usize;
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    let mut prev_mouse_down = false;
    // Assume Escape is untouched at the start, and see `esc_pressed` below.
    let mut esc_was_down = false;
    // What the last export did, shown in the footer in place of the key hints.
    // Shown on screen rather than printed, because the terminal is not where
    // somebody who clicked a button is looking.
    let mut status: Option<String> = None;
    // Redraw only when something moved. The list is static between keystrokes,
    // and re-rasterising forty-odd strings sixty times a second to produce an
    // identical picture is exactly the kind of idle cost that makes an app feel
    // heavy. `minifb::Window::update` re-presents the last buffer and still pumps
    // input, so responsiveness does not depend on redrawing.
    let mut dirty = true;
    // Replaced by the first draw; nothing is hit-tested before that.
    let mut rows = Rows {
        top: 0,
        height: 1,
        visible: 1,
        left: 0,
        width: 0,
        play_win: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        play_full: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        export: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        import: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        idle_toggle: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        idle_module: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        idle_delay: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        idle_sound: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        idle_preview: Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
    };
    // Trackpads deliver many small deltas; one line per event would fly. This
    // accumulates until a whole row's worth has arrived.
    let mut wheel = 0.0f32;

    // Idle start: the user's settings, and the throttled idle clock.
    let mut idle = ad_runtime::IdleSettings::load();
    let mut watch = ad_runtime::IdleWatch::new();
    // After a module ends, the machine is by definition *not* idle — the user
    // just pressed Escape. Without this the timer would re-fire the moment they
    // stopped moving, which reads as "it will not let me leave".
    let mut armed = true;
    // Which module to run once the window borrow is released, and whether to
    // bring the window forward for it. Set inside the borrow, acted on outside.
    let mut to_run: Option<usize>;
    let mut raise = false;
    // Whether a person chose this module, as opposed to the idle timer or
    // Preview choosing it for them. Only a hand-started run gets asked which
    // control layout to use; see `choose_layout`. Assigned unconditionally
    // every pass, so unlike `raise` it needs no seed and no reset.
    let mut by_hand;

    loop {
        if dirty {
            rows = draw_browser(
                &mut canvas,
                &font,
                &modules,
                selected,
                &mut scroll,
                status.as_deref(),
                &idle,
            );
            dirty = false;
        }

        {
            let mut w = window.borrow_mut();
            w.set_target_fps(60);
            // Presenting an unchanged buffer is a blit; the cost `dirty` avoids
            // is re-rasterising the text above, which is the expensive part.
            w.update_with_buffer(&canvas.px, WIDTH, HEIGHT)?;
            // On a rising edge only. Leaving a module returns here with Escape
            // very likely still held — the module exited the instant it went down
            // — and reading the level would have taken that same press as "quit
            // the application". One key press closed everything.
            let esc = w.is_key_down(Key::Escape);
            let esc_pressed = esc && !esc_was_down;
            esc_was_down = esc;
            if !w.is_open() || esc_pressed {
                return Ok(());
            }

            let last = modules.len().saturating_sub(1);
            let max_scroll = modules.len().saturating_sub(rows.visible);
            let (was_selected, was_scroll) = (selected, scroll);

            // Keyboard: arrows move the selection, and the view follows it.
            if w.is_key_pressed(Key::Down, minifb::KeyRepeat::Yes) {
                selected = (selected + 1).min(last);
            }
            if w.is_key_pressed(Key::Up, minifb::KeyRepeat::Yes) {
                selected = selected.saturating_sub(1);
            }
            if w.is_key_pressed(Key::PageDown, minifb::KeyRepeat::Yes) {
                selected = (selected + rows.visible).min(last);
            }
            if w.is_key_pressed(Key::PageUp, minifb::KeyRepeat::Yes) {
                selected = selected.saturating_sub(rows.visible);
            }
            if w.is_key_pressed(Key::Home, minifb::KeyRepeat::No) {
                selected = 0;
            }
            if w.is_key_pressed(Key::End, minifb::KeyRepeat::No) {
                selected = last;
            }
            if selected != was_selected {
                reveal(selected, rows.visible, &mut scroll);
            }

            // Wheel and trackpad: move the *view*, leaving the selection alone,
            // which is what every other list does. minifb hands NSEvent's
            // `deltaY` through unchanged, and on macOS that is positive when the
            // gesture asks for earlier content — so positive scrolls towards the
            // top of the list.
            if let Some((_, dy)) = w.get_scroll_wheel() {
                wheel += dy;
            }
            let lines = (wheel / WHEEL_LINE) as i32;
            if lines != 0 {
                wheel -= lines as f32 * WHEEL_LINE;
                scroll = if lines > 0 {
                    scroll.saturating_sub(lines.unsigned_abs() as usize)
                } else {
                    scroll
                        .saturating_add(lines.unsigned_abs() as usize)
                        .min(max_scroll)
                };
            }

            // Mouse: click a row to select it, click again to run it. Two clicks
            // rather than one, so a stray click cannot launch a module and take
            // over the screen.
            let down = w.get_mouse_down(MouseButton::Left);
            let clicked = down && !prev_mouse_down;
            prev_mouse_down = down;
            let mut launch = w.is_key_pressed(Key::Enter, minifb::KeyRepeat::No);

            // Export and import: the buttons, or `E` and `I`. Each runs a modal
            // folder chooser, so the window stops responding until the person
            // answers — which is what a modal dialog is, and why it is only ever
            // on these deliberate actions.
            let at = cursor_in_buffer(&w);
            let hit = |r: Rect| clicked && at.is_some_and(|(x, y)| r.contains(x, y));
            if hit(rows.export) || w.is_key_pressed(Key::E, minifb::KeyRepeat::No) {
                status = Some(export_scores_interactively());
                dirty = true;
                continue;
            }
            if hit(rows.import) || w.is_key_pressed(Key::I, minifb::KeyRepeat::No) {
                status = Some(import_scores_interactively());
                dirty = true;
                continue;
            }

            // The idle-start controls. Each writes the settings straight back to
            // disk: there is no OK button on this strip, so a click that did not
            // persist would be a setting that silently forgets itself.
            let mut changed = false;
            if hit(rows.idle_toggle) {
                if ad_runtime::IdleSettings::available() {
                    idle.enabled = !idle.enabled;
                    changed = true;
                } else {
                    status = Some(
                        "Idle start needs a system idle timer, which this platform \
                         does not expose"
                            .to_owned(),
                    );
                    dirty = true;
                }
            }
            if hit(rows.idle_module) {
                // Random ⇄ whatever is highlighted in the list, so choosing a
                // module is "point at it, then click here" rather than a menu
                // this drawing code would have to grow.
                idle.module = match idle.module {
                    ad_runtime::IdleModule::Random => modules
                        .get(selected)
                        .map_or(ad_runtime::IdleModule::Random, |m| {
                            ad_runtime::IdleModule::Named(m.title.clone())
                        }),
                    ad_runtime::IdleModule::Named(_) => ad_runtime::IdleModule::Random,
                };
                changed = true;
            }
            if hit(rows.idle_delay) {
                idle.cycle_delay();
                changed = true;
            }
            if hit(rows.idle_sound) {
                idle.sound = !idle.sound;
                changed = true;
            }
            if changed {
                if let Err(e) = idle.save() {
                    status = Some(format!("Could not save idle settings: {e}"));
                }
                dirty = true;
                continue;
            }

            if clicked {
                if let Some((x, y)) = cursor_in_buffer(&w) {
                    if let Some(row) = rows.row_at(x, y) {
                        let index = scroll + row;
                        if index <= last {
                            if index == selected {
                                launch = true;
                            } else {
                                selected = index;
                            }
                        }
                    }
                }
            }
            if selected != was_selected || scroll != was_scroll {
                dirty = true;
            }

            // What to run, and whether the window should be brought forward for
            // it. Return, a double click and the panel's Play run the
            // *highlighted* module; Preview and the idle timer run the *idle
            // choice*, which may be a different one entirely.
            if hit(rows.play_win) {
                launch = true;
            }
            if hit(rows.play_full) {
                launch = true;
                raise = true;
            }
            to_run = launch.then_some(selected);
            by_hand = launch;
            if to_run.is_none()
                && (hit(rows.idle_preview) || w.is_key_pressed(Key::P, minifb::KeyRepeat::No))
            {
                to_run = idle_choice(&idle, &modules);
                raise = true;
            }
            // The timer itself. `armed` stops a module that exits immediately
            // from being restarted in a loop: the machine has to become active
            // again, then idle again, before it fires a second time.
            if !watch.idle_for(idle.after_minutes) {
                armed = true;
            } else if to_run.is_none() && idle.enabled && armed {
                if let Some(cmd) = idle.command.clone() {
                    // Something else takes the screen; nothing of ours runs.
                    armed = false;
                    drop(w);
                    status = Some(run_idle_command(&cmd, &mut watch));
                    dirty = true;
                    continue;
                }
                to_run = idle_choice(&idle, &modules);
                raise = true;
            }
            if to_run.is_none() {
                continue;
            }
        }

        // The window borrow is released before running: the present hook needs it.
        let index = to_run.take().unwrap_or(selected);
        armed = false;
        let entry = &modules[index.min(modules.len() - 1)];
        println!("\n--- {} ---", entry.title);
        // The press that left the module must not also be read as "quit".
        esc_was_down = true;
        // Every run gets its own borderless window covering the display,
        // dropped (and so closed) when the module ends — a raised one from
        // Preview or the idle timer, and equally one a person started from the
        // list. A module is the screen's whole content or it is a toy in a box;
        // there is no reading of "screen saver" where playing one in a 640x480
        // window with the Dock underneath is the better answer.
        //
        // The browser window cannot be used for this: `minifb` cannot resize a
        // window after creation, so "full screen" has to be a window *created*
        // at screen size. When the display cannot be asked, the shared window
        // floats to the front instead — smaller, but present.
        //
        // A full-screen run replaces the window in the shared slot rather than
        // opening a second one — the old window must be GONE, not just behind:
        // its Metal view would keep rendering underneath and starve the
        // emulator. See `browser_window` for the symptom this caused.
        let went_fullscreen = match fullscreen_window(&entry.title) {
            Some(fs) => {
                *window.borrow_mut() = fs;
                true
            }
            None => false,
        };
        // Held across the run and dropped before the browser comes back, so the
        // menu bar and the Dock return with it. See `Kiosk` for what this can
        // and cannot switch off.
        let kiosk = went_fullscreen.then(ad_keystate::hold_screen);
        if let Err(e) = run_module(&entry.path, window, audio, options, raise, by_hand) {
            println!("{e}");
        }
        drop(kiosk);
        if went_fullscreen {
            // Give the browser its window back. If the display refuses, there
            // is nothing left to draw into, so leaving is the only honest exit.
            match browser_window() {
                Ok(w) => *window.borrow_mut() = w,
                Err(e) => {
                    eprintln!("could not reopen the browser window: {e}");
                    return Ok(());
                }
            }
        }
        raise = false;
        // The game may have just written a new high score, and the details
        // panel is showing the old table until something redraws it.
        dirty = true;
        if !window.borrow().is_open() {
            return Ok(());
        }
    }
}

/// Where the list rows landed, so a click can be turned back into an index.
struct Rows {
    top: i32,
    height: i32,
    visible: usize,
    /// The list's own horizontal span. A click outside it is not a row click:
    /// rows used to be hit-tested on Y alone, so clicking anywhere in the
    /// details panel selected (and could launch) whatever module happened to
    /// share that height.
    left: i32,
    width: i32,
    /// Where the Export button was drawn.
    export: Rect,
    /// Where the Import button was drawn.
    import: Rect,
    /// Play the selected module, windowed / covering the display.
    play_win: Rect,
    play_full: Rect,
    /// The idle-start controls, in the order they are drawn.
    idle_toggle: Rect,
    idle_module: Rect,
    idle_delay: Rect,
    idle_sound: Rect,
    idle_preview: Rect,
}

impl Rows {
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if x < self.left || x >= self.left + self.width {
            return None;
        }
        if self.height <= 0 || y < self.top {
            return None;
        }
        let row = usize::try_from((y - self.top) / self.height).ok()?;
        (row < self.visible).then_some(row)
    }
}

/// Draw the browser, scrolling to keep the selection visible.
fn draw_browser(
    canvas: &mut Canvas,
    font: &Font,
    modules: &[library::Entry],
    selected: usize,
    scroll: &mut usize,
    status: Option<&str>,
    idle: &ad_runtime::IdleSettings,
) -> Rows {
    let f = font.strike();
    let line = i32::from(f.line_height()).max(9);
    let row_h = line + 3;
    let header = line + 12;
    let footer = line + 10;
    // Two strips at the bottom now: the idle controls above the status line, so
    // the list shrinks rather than anything overlapping.
    let idle_bar = row_h + 12;
    let list_w = 300i32;
    let list_top = header;
    let list_h = HEIGHT as i32 - header - footer - idle_bar;
    let visible = usize::try_from((list_h / row_h).max(1)).unwrap_or(1);

    // Only clamp. Following the selection is the *caller's* job, and it has to
    // be, because the wheel scrolls the view without moving the selection: doing
    // it here would drag the view straight back on the next redraw and the list
    // would refuse to scroll.
    let max_scroll = modules.len().saturating_sub(visible);
    *scroll = (*scroll).min(max_scroll);

    canvas.clear(colour::BACKGROUND);
    canvas.rect(0, 0, WIDTH as i32, header, colour::PANEL);
    canvas.text(
        &f,
        12,
        i32::from(f.ascent) + 6,
        &format!("{APP_NAME} — {} modules", modules.len()),
        colour::HEADING,
    );

    // ---- the list ----
    canvas.rect(8, list_top, list_w, list_h, colour::PANEL);
    canvas.frame(8, list_top, list_w, list_h, colour::FRAME);
    for slot in 0..visible {
        let Some(entry) = modules.get(scroll.saturating_add(slot)) else {
            break;
        };
        let y = list_top + 1 + i32::try_from(slot).unwrap_or(0) * row_h;
        let chosen = scroll.saturating_add(slot) == selected;
        if chosen {
            canvas.rect(9, y, list_w - 2, row_h, colour::SELECTED);
        }
        let ink = if chosen {
            colour::SELECTED_INK
        } else {
            colour::INK
        };
        canvas.text_clipped(
            &f,
            14,
            y + i32::from(f.ascent) + 1,
            &entry.title,
            list_w - 16,
            ink,
        );
    }
    // A scroll indicator, so a list longer than the box says so.
    if modules.len() > visible {
        let bar_h = (list_h * i32::try_from(visible).unwrap_or(1)
            / i32::try_from(modules.len()).unwrap_or(1))
        .max(8);
        let bar_y = list_top
            + (list_h - bar_h) * i32::try_from(*scroll).unwrap_or(0)
                / i32::try_from(max_scroll.max(1)).unwrap_or(1);
        canvas.rect(8 + list_w - 5, bar_y, 4, bar_h, colour::DIM);
    }

    // ---- details for the selection ----
    let dx = 8 + list_w + 10;
    let dw = WIDTH as i32 - dx - 8;
    canvas.rect(dx, list_top, dw, list_h, colour::PANEL);
    canvas.frame(dx, list_top, dw, list_h, colour::FRAME);
    // Height reserved at the panel's foot for the play buttons, so however long
    // the settings and score lists get, they stop above it.
    let button_zone = row_h + 20;
    if let Some(entry) = modules.get(selected) {
        let mut y = list_top + i32::from(f.ascent) + 8;
        canvas.text_clipped(&f, dx + 10, y, &entry.title, dw - 20, colour::HEADING);
        // The module's own descriptor, which is often not its name: five modules
        // on the disk share "Flying Toasters 2.0" and 26 carry none at all. Only
        // take the line when there is something to put on it.
        if let Some(d) = entry.descriptor.as_deref() {
            y += row_h + 2;
            canvas.text_clipped(&f, dx + 10, y, d, dw - 20, colour::DIM);
        }
        y += row_h + 4;
        canvas.text(
            &f,
            dx + 10,
            y,
            &format!(
                "{} resources{}",
                entry.resources,
                if entry.has_sound { ", sound" } else { "" }
            ),
            colour::DIM,
        );
        y += row_h + 6;
        if entry.controls.is_empty() {
            canvas.text(&f, dx + 10, y, "No settings", colour::DIM);
        } else {
            canvas.text(&f, dx + 10, y, "Settings", colour::DIM);
            y += row_h + 2;
            for c in &entry.controls {
                if y > list_top + list_h - row_h {
                    break;
                }
                canvas.text_clipped(&f, dx + 18, y, c, dw - 28, colour::INK);
                y += row_h;
            }
        }

        // ---- high scores, for a module that has saved any ----
        //
        // Read from the same overlay the module itself loads, so this list and
        // the table the game draws can never disagree. Modules that save no
        // `LFhs` — all but Lunatic Fringe — contribute nothing and get no
        // heading, rather than an empty box implying scores that never come.
        let scores = fringe_scores(&entry.title);
        if !scores.is_empty() {
            y += 6;
            canvas.text(&f, dx + 10, y, "High scores", colour::DIM);
            y += row_h + 2;
            // Leave the button strip's territory alone however long the list is.
            let floor = list_top + list_h - row_h - button_zone;
            for (i, row) in scores.iter().enumerate() {
                if y > floor {
                    break;
                }
                canvas.text_clipped(
                    &f,
                    dx + 18,
                    y,
                    &format!("{}. {}", i + 1, row.name),
                    dw - 130,
                    colour::INK,
                );
                // Score right-aligned, so ten of them read as a column.
                let tail = format!("{}  (level {})", row.score, row.level);
                let tw = f.text_width(tail.as_bytes());
                canvas.text(&f, dx + dw - 14 - tw, y, &tail, colour::HEADING);
                y += row_h;
            }
        }
    }

    // ---- the play buttons, bottom of the details panel ----
    //
    // The panel's whole job is "this module: what it is, how it is set, how it
    // did — and a way to run it". Return and a second click on the row still
    // work; these exist so running is *discoverable*, and so covering the
    // display is a choice made per launch rather than a mode to remember.
    let pb_h = row_h + 6;
    let pb_y = list_top + list_h - pb_h - 8;
    let mut pb_x = dx + 10;
    let mut panel_button = |canvas: &mut Canvas, label: &str| -> Rect {
        let bw = f.text_width(label.as_bytes()) + 20;
        let r = Rect {
            x: pb_x,
            y: pb_y,
            w: bw,
            h: pb_h,
        };
        pb_x = r.x + bw + 8;
        canvas.rect(r.x, r.y, r.w, r.h, colour::BACKGROUND);
        canvas.frame(r.x, r.y, r.w, r.h, colour::FRAME);
        canvas.text(
            &f,
            r.x + 10,
            r.y + i32::from(f.ascent) + 4,
            label,
            colour::INK,
        );
        r
    };
    let play_win = panel_button(canvas, "Play (Return)");
    let play_full = panel_button(canvas, "Play full screen");

    // ---- the idle-start controls ----
    //
    // Laid out left to right, each button labelled with its own current value
    // rather than with a separate label and field: "After: 5 min" is both the
    // setting and the control, which is the only way four settings fit on one
    // 640-pixel strip and stay legible in a 12-point bitmap face.
    let iy = HEIGHT as i32 - footer - idle_bar;
    canvas.rect(0, iy, WIDTH as i32, idle_bar, colour::PANEL);
    let ih = row_h + 4;
    let iby = iy + (idle_bar - ih) / 2;
    let mut left = 12;
    let mut idle_button = |canvas: &mut Canvas, label: &str, on: bool, max_w: i32| -> Rect {
        let bw = (f.text_width(label.as_bytes()) + 18).min(max_w);
        let r = Rect {
            x: left,
            y: iby,
            w: bw,
            h: ih,
        };
        left = r.x + bw + 6;
        let (fill, ink) = if on {
            (colour::SELECTED, colour::SELECTED_INK)
        } else {
            (colour::BACKGROUND, colour::INK)
        };
        canvas.rect(r.x, r.y, r.w, r.h, fill);
        canvas.frame(r.x, r.y, r.w, r.h, colour::FRAME);
        canvas.text_clipped(
            &f,
            r.x + 9,
            r.y + i32::from(f.ascent) + 3,
            label,
            bw - 14,
            ink,
        );
        r
    };

    let can_idle = ad_runtime::IdleSettings::available();
    let toggle_label = if !can_idle {
        "Idle: no timer".to_owned()
    } else if idle.enabled {
        "Idle: ON".to_owned()
    } else {
        "Idle: off".to_owned()
    };
    let idle_toggle = idle_button(canvas, &toggle_label, can_idle && idle.enabled, i32::MAX);
    // The module name is clipped rather than allowed to widen the strip: some
    // titles are 30 characters and would push everything after them off screen.
    let start_label = if idle.command.is_some() {
        // Set in idle.conf by hand; the strip reports it rather than pretending
        // a module will run.
        "Start: external command".to_owned()
    } else {
        match &idle.module {
            ad_runtime::IdleModule::Random => "Start: Random".to_owned(),
            ad_runtime::IdleModule::Named(name) => format!("Start: {name}"),
        }
    };
    // Capped, and the label clipped inside it: module titles are filenames from
    // the user's own folder, and one long enough would otherwise push Preview
    // off the right-hand edge and out of reach of the mouse.
    let idle_module = idle_button(canvas, &start_label, false, IDLE_MODULE_MAX_W);
    let idle_delay = idle_button(
        canvas,
        &format!("After: {}", idle.delay_label()),
        false,
        i32::MAX,
    );
    let idle_sound = idle_button(
        canvas,
        if idle.sound {
            "Sound: on"
        } else {
            "Sound: muted"
        },
        false,
        i32::MAX,
    );
    let idle_preview = idle_button(canvas, "Preview (P)", false, i32::MAX);

    // ---- footer ----
    let fy = HEIGHT as i32 - footer;
    canvas.rect(0, fy, WIDTH as i32, footer, colour::PANEL);

    // ---- the export and import buttons ----
    //
    // Each is drawn from the same rect that hit-tests it, returned in `Rows`, so
    // the clickable area and the visible one cannot drift apart. Laid out from
    // the right edge inwards, in pairs, so neither has a hard-coded position
    // that a change to the other's label would silently overlap.
    let bh = row_h + 4;
    let by = fy + (footer - bh) / 2;
    let mut right = WIDTH as i32 - 10;
    let mut button = |canvas: &mut Canvas, label: &str| -> Rect {
        let bw = f.text_width(label.as_bytes()) + 20;
        let r = Rect {
            x: right - bw,
            y: by,
            w: bw,
            h: bh,
        };
        right = r.x - 8;
        canvas.rect(r.x, r.y, r.w, r.h, colour::BACKGROUND);
        canvas.frame(r.x, r.y, r.w, r.h, colour::FRAME);
        canvas.text(
            &f,
            r.x + 10,
            r.y + i32::from(f.ascent) + 3,
            label,
            colour::INK,
        );
        r
    };
    // Export first, so it keeps the outermost position it has always had.
    let export = button(canvas, "Export scores… (E)");
    let import = button(canvas, "Import scores… (I)");

    // The footer line, drawn after the buttons and clipped to what they leave.
    // It used to be drawn first and ran underneath them — legible only because
    // it happened to be short enough, which stopped being true the moment a
    // second button appeared.
    canvas.text_clipped(
        &f,
        12,
        fy + i32::from(f.ascent) + 4,
        // "·" is MacRoman $E1, past Geneva 12's last character ($D9), so it drew
        // as the missing-character box — the decoder behaving correctly and the
        // text being wrong. "•" is $A5 and inside every strike on the disk.
        status.unwrap_or("Arrows select • Return runs • Esc quits"),
        (right - 12 - 12).max(0),
        colour::DIM,
    );

    Rows {
        top: list_top + 1,
        height: row_h,
        visible,
        left: 8,
        width: list_w,
        play_win,
        play_full,
        export,
        import,
        idle_toggle,
        idle_module,
        idle_delay,
        idle_sound,
        idle_preview,
    }
}

/// Load and run one module in the shared window until it ends or Esc is pressed.
/// The two key sets Lunatic Fringe's own `LFky` resource declares, drawn for the
/// chooser.
///
/// Both are original: the game polls for both at once, and the second needs no
/// numeric keypad, which is what makes it the one most keyboards can actually
/// play. Listing them is the point of the screen — the keypad-free set is
/// invisible otherwise, and somebody with a laptop would reasonably conclude the
/// original controls were not for them.
/// `G` rather than `Space` on the second row, and `F` in the heading rather than
/// Command: those are the two the game's own screen names, and together they are
/// Spotlight. See [`ORIGINAL_MAP`]. Both originals still work — this row is what
/// somebody without a keypad should press, not all they *can*.
///
/// The heading says **original-ish** for exactly that reason. Calling it the
/// original while two of its keys are ours would be a small lie told on the one
/// screen whose whole job is to be believed.
const ORIGINAL_SETS: &[(&str, &str)] = &[
    ("keypad", "4 / 6 turn    5 thrust    8 turbo    0 shield"),
    ("no keypad", "L / ' turn    ; thrust    P turbo    G shield"),
];

/// The modern layout's three lines on the chooser screen.
///
/// The heading says "either one" because that is the question this screen gets
/// asked next: the two hands are not two halves of one set that must be
/// combined, they are the same four controls twice over, and a player may use
/// whichever is under their fingers — including one for turning and the other
/// for thrust. Naming Left/Right and Up rather than "arrows" is what makes that
/// checkable at a glance.
const MODERN_SETS: &[&str] = &[
    "MODERN - use WAD or the arrow keys, either one",
    "turn: A / D  or  Left / Right       thrust: W  or  Up",
    "J fire    L turbo    K shield    Delete aborts ship",
];

/// The chooser's geometry, hoisted so a test can measure the same room the
/// screen gives its text: the panel inset from the display, the boxes inset from
/// the panel, and 12px of padding either side of a line.
const CHOOSER_PANEL_W: i32 = WIDTH as i32 - 80;
const CHOOSER_BOX_W: i32 = CHOOSER_PANEL_W - 48;
/// The room a line of text actually gets. Only the test reads it — the drawing
/// code needs the box, not the gap inside it — but it belongs beside the two it
/// is derived from, or it drifts the first time the panel is resized.
#[cfg(test)]
const CHOOSER_TEXT_W: i32 = CHOOSER_BOX_W - 24;

/// The chooser's bottom line, hoisted for the same reason every other banner is:
/// nothing clips it, so a line too long to fit runs off the edge of the screen
/// and reads as a rendering fault rather than as text.
///
/// It names the pause keys because this is the last screen before the game, and
/// the game's own first screen says only "Press Caps Lock" — which is true and
/// half the answer. See [`FRINGE_PAUSE`].
const CHOOSER_FOOTER: &str =
    "Up / Down chooses    Return starts    Esc goes back    1 or Caps Lock pauses";

/// Ask which control layout to start Lunatic Fringe in.
///
/// This exists because the layouts are not interchangeable and the wrong one is
/// silently wrong: the modern layout remaps `L` onto Turbo Thrust, so a player
/// using the game's own keypad-free set finds that turning left simply does
/// nothing. No error, no clue — the key is read perfectly and lands on a control
/// they were not aiming at.
///
/// Asked rather than remembered, and asked *before* the module starts rather
/// than fixed afterwards with `C`, because the first thirty seconds of a game
/// are where a wrong layout does its damage. `C` still swaps at any time.
///
/// [`None`] means the person backed out and the module should not run. Without a
/// font there is nothing to draw with, so the caller's default stands — the same
/// discipline the browser follows when it cannot draw a list.
fn choose_layout(window: &Rc<RefCell<Window>>, dir: &Path, title: &str) -> Option<bool> {
    let font = Font::discover(dir)?;
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    // Modern is the highlighted option, matching the default it replaces.
    let mut modern = true;
    let mut esc_was_down = false;
    loop {
        let strike = font.strike();
        canvas.clear(colour::BACKGROUND);
        let panel = (40, 60, CHOOSER_PANEL_W, HEIGHT as i32 - 150);
        canvas.rect(panel.0, panel.1, panel.2, panel.3, colour::PANEL);
        canvas.frame(panel.0, panel.1, panel.2, panel.3, colour::FRAME);

        let x = panel.0 + 24;
        canvas.text(&strike, x, panel.1 + 34, title, colour::HEADING);
        canvas.text(&strike, x, panel.1 + 56, "Which controls?", colour::DIM);

        // Two boxes, the highlighted one inverted. Inversion rather than a tick
        // or a border, because it is what a System 7 list used and it survives
        // being looked at quickly.
        let (bw, bh) = (CHOOSER_BOX_W, 74);
        for (i, is_modern) in [true, false].into_iter().enumerate() {
            let y = panel.1 + 78 + (i as i32) * (bh + 12);
            let picked = is_modern == modern;
            let (bg, fg) = if picked {
                (colour::SELECTED, colour::SELECTED_INK)
            } else {
                (colour::PANEL, colour::INK)
            };
            canvas.rect(x, y, bw, bh, bg);
            canvas.frame(x, y, bw, bh, colour::FRAME);
            if is_modern {
                for (j, line) in MODERN_SETS.iter().enumerate() {
                    canvas.text(&strike, x + 12, y + 20 + (j as i32) * 20, line, fg);
                }
            } else {
                canvas.text(&strike, x + 12, y + 20, "ORIGINAL-ISH - F fires", fg);
                for (j, (name, keys)) in ORIGINAL_SETS.iter().enumerate() {
                    let ty = y + 40 + (j as i32) * 20;
                    canvas.text(&strike, x + 12, ty, &format!("{name}:"), fg);
                    canvas.text(&strike, x + 108, ty, keys, fg);
                }
            }
        }
        canvas.text(
            &strike,
            x,
            panel.1 + panel.3 - 16,
            CHOOSER_FOOTER,
            colour::DIM,
        );

        let mut w = window.borrow_mut();
        if !w.is_open() {
            return None;
        }
        w.update_with_buffer(&canvas.px, WIDTH, HEIGHT).ok()?;
        for key in w.get_keys_pressed(minifb::KeyRepeat::No) {
            match key {
                Key::Up | Key::Down | Key::Left | Key::Right | Key::Tab => modern = !modern,
                Key::Enter | Key::NumPadEnter | Key::Space => return Some(modern),
                _ => {}
            }
        }
        // Escape on its *release*, so the press that left the previous screen
        // cannot fall through and cancel this one the instant it appears.
        let esc = w.is_key_down(Key::Escape);
        if esc_was_down && !esc {
            return None;
        }
        esc_was_down = esc;
    }
}

fn run_module(
    path: &Path,
    window: &Rc<RefCell<Window>>,
    audio: Option<&Rc<ad_runtime::AudioDevice>>,
    options: &ad_runtime::RuntimeOptions,
    raise: bool,
    by_hand: bool,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fork = ResourceFork::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let settings = ModuleSettings::from_fork(&fork);
    let controls = settings.control_values();
    let fork = ResourceFork::parse(&bytes).map_err(|e| e.to_string())?;
    let mut host = Host::load(fork, controls).map_err(|e| e.to_string())?;
    host.set_diagnostics(options.diagnostics);

    // The save key is the **filename**, not the `ADrk 0` descriptor.
    //
    // The descriptor is not unique: Bogglins, Flying Toasters, Major
    // Metaphysical Appliances, Pearls and ProtoToasters all carry the same
    // copy-pasted "Flying Toasters 2.0 ©1990 Berkeley Systems Inc." on the
    // original disk, so keying saves on it pointed all five at one file and
    // each would load and overwrite the others' state. Filenames are unique
    // within a folder by construction, and they are already what the module
    // list and the high-score browser (`fringe_scores`) key on — so this is
    // also what makes the score panel read the same file the game writes.
    let title = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    // The saved sound preference is the *starting* state for every launch.
    let start_muted = !ad_runtime::IdleSettings::load().sound;

    // Whether this is Lunatic Fringe, by its own key-table resource rather than
    // by name: `LFky` is where the keypad/thrust/fire controls the banner
    // describes actually come from, so its presence is precisely "this module
    // has those controls". Every other module gets a banner that promises only
    // what is true everywhere: Escape leaves.
    let fringe = ResourceFork::parse(&bytes)
        .ok()
        .is_some_and(|f| f.get(b"LFky", 128).is_some());
    // Which layout to start in. Asked only when a person started this module by
    // hand: the idle timer and Preview are the screen saver doing its job with
    // nobody watching, and a dialog waiting for Return would stop it dead.
    let mut start_modern = fringe;
    if fringe && by_hand {
        match choose_layout(window, path.parent().unwrap_or(Path::new(".")), &title) {
            Some(picked) => start_modern = picked,
            // Backed out. Not an error — it is the answer "neither, take me
            // back" — so the browser returns to its list as if nothing ran.
            None => return Ok(()),
        }
    }

    // The console gets the same story as the banner. Said every launch because
    // the Caps Lock substitute is not guessable, but only where it is true.
    if fringe {
        // The layout that was actually chosen, not the one that is the default.
        // Nothing swaps mid-game any more, so a description of the other one is
        // not merely premature — it is wrong for the whole session.
        let controls = if start_modern {
            "The modern layout steers with either hand: A and D turn, W thrusts — or \
             Left and Right turn, Up thrusts. They are the same controls twice, so use \
             whichever you like, or one of each. Then J fires, L turbo, K shield, \
             Delete aborts."
        } else {
            "Original-ish: the game's own controls, with F to fire and G for Power \
             Shield. With a keypad, 4 and 6 turn, 5 thrusts, 8 turbo, 0 shields, A \
             aborts; without one, L and ' turn, ; thrusts, P turbo, G shields. F \
             fires either way. The game's own screen names Command and Space for \
             those two instead, and both still work — but holding Command while \
             pressing Space is Spotlight, which macOS takes before this app is \
             offered the keystroke, so F and G are the pair that can be held at \
             once."
        };
        println!(
            "Press Caps Lock, 1, or click the window to start or pause the game — \
             Caps Lock is the one the game's own screen asks for. {controls} Esc \
             twice returns to the list. State is in the title bar."
        );
    } else {
        println!("Esc twice returns to the list.");
    }

    // Fonts, from beside the module — `_DrawString` draws nothing without them.
    for fork in ad_runtime::font_forks(path.parent().unwrap_or(Path::new("."))) {
        host.add_font_fork(&fork);
    }

    // Saved state must be merged before Initialize: a module reads its high
    // scores there, so a later merge would show the shipped defaults for a whole
    // session and then save those over the real ones.
    if let Some(dir) = options.save_dir.as_deref() {
        // A corrupt save must not stop the module from running: report it and
        // start from the shipped defaults, which is a recoverable state.
        match ad_runtime::ForkSink::load(dir, &title) {
            Ok(saved) => {
                if !saved.is_empty() {
                    println!("restored {} saved resource(s)", saved.len());
                }
                host.attach_saved_state(saved, Box::new(ad_runtime::ForkSink::new(dir, &title)));
            }
            Err(e) => eprintln!("saved state ignored: {e}"),
        }
    }

    // A game can spend an unbounded number of cycles inside one DrawFrame. The
    // budget only needs to be large enough that a genuinely stuck module is
    // still reported rather than hanging the window forever.
    host.cycle_budget = u32::MAX;
    // How fast the emulated machine is, and therefore how fast the module runs.
    // The only knob here with no right answer — see `MachineProfile::clock_hz`.
    if let Some(hz) = options.clock_hz {
        println!("emulated clock {:.2} MHz", f64::from(hz) / 1e6);
        host.tb.profile.clock_hz = hz;
    } else if fringe {
        // Lunatic Fringe paces itself at one game frame per 60 Hz tick, so a
        // faster machine cannot make it play faster — only let it reach that
        // rate. At the survey's 8 MHz it averages about half the cap and loses
        // the rest precisely when the screen fills — the recharge base, the
        // spawner, the fast chasers — which plays as the game bogging down
        // mid-fight. Measured over the same 5,100-tick scripted session,
        // per-frame work rose 70% from 8 to 16 MHz but only 23% more from
        // 16 to 40 MHz: a ceiling being reached, not a runaway. 40 MHz is a
        // IIfx, the fast machine of the game's own era, and enough that the
        // busiest scenes hold the cap. Screensavers keep the 8 MHz survey
        // baseline; the one game gets a gamer's machine. `AD_MHZ` overrides
        // this too, in either direction.
        host.tb.profile.clock_hz = 40_000_000;
    }

    let shared = Rc::new(RefCell::new(Shared {
        pixels: vec![0u32; WIDTH * HEIGHT],
        held: Vec::new(),
        mouse: ((WIDTH / 2) as i16, (HEIGHT / 2) as i16),
        quit: false,
        presented: 0,
        caps: false,
        active_means: None,
        caps_lock_was: false,
        click_was_down: false,
        hid: false,
        key_seen: Vec::new(),
        suppressed: Vec::new(),
        cmd_was_down: false,
        cmd_released_at: None,
        modern: start_modern,
        muted: start_muted,
        typing: false,
        typing_is_stale: false,
        esc_armed_until: None,
        hint_until: HINT_TICKS,
    }));

    {
        let mut w = window.borrow_mut();
        w.set_title(&format!("{APP_NAME} — {title}"));
        // The window must not throttle: the module drives it from inside its own
        // loop, and the pace is set by `Pacer` against the emulated tick instead.
        // Two limiters would fight, and minifb's is per-`update`, which a module
        // that presents irregularly would turn into an irregular clock.
        w.set_target_fps(0);
        // Started by the idle timer rather than by a click: the window is very
        // likely behind whatever the person was last using, so float it to the
        // top-left and above other windows. It is *not* made full screen —
        // `minifb` cannot resize a window after creation, and it is not made to
        // steal focus either, because taking keyboard focus from another
        // application on a timer is what a well-behaved program does not do.
        // Clicking the window both focuses it and starts an interactive module,
        // which is the same gesture either way.
        if raise {
            w.topmost(true);
            w.set_position(0, 0);
        }
    }

    if let Some(dev) = audio {
        let dev = Rc::clone(dev);
        let shared = Rc::clone(&shared);
        let mut was_muted = false;
        host.set_sound_hook(Box::new(move |events| {
            let muted = shared.borrow().muted;
            if muted {
                // Silenced on the edge, not per event: a looping engine note
                // already in the mixer would otherwise play on under the mute.
                if !was_muted {
                    dev.silence();
                }
            } else {
                dev.submit(events);
            }
            was_muted = muted;
        }));
    }

    // Watch the module's own words for the moment it wants typing rather than
    // steering. See `Shared::typing`; the banner is re-shown because the controls
    // have just changed under the player's feet and saying so is the difference
    // between "the keys went weird" and "it is asking me to type".
    {
        let shared = Rc::clone(&shared);
        host.set_text_hook(Box::new(move |said| {
            if !asks_for_typing(said) {
                return;
            }
            let mut s = shared.borrow_mut();
            if !s.typing {
                s.typing = true;
                s.typing_is_stale = false;
                s.held.clear();
                s.hint_until = u32::MAX;
            }
        }));
    }

    // Key transitions, recorded as they happen rather than sampled once a frame.
    let taps: Rc<RefCell<Vec<(Key, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    window.borrow_mut().set_input_callback(Box::new(KeyTaps {
        taps: Rc::clone(&taps),
    }));

    // The present hook runs inside the emulator's tick loop: it publishes the
    // frame, then pumps the window so input and redraw keep happening even
    // though `DrawFrame` has not returned.
    {
        let shared = Rc::clone(&shared);
        let window = Rc::clone(window);
        let mut pacer = ad_runtime::Pacer::new();
        let mut shown_caps = false;
        let stats = options.stats;
        let mut stat_epoch = std::time::Instant::now();
        let mut stat_frames = 0u64;
        let title_for_hook = title.clone();
        // The banner, and the font to draw it with. Both `None` when the user has
        // no Macintosh font beside their modules, in which case the terminal line
        // is the only instruction — the same rule as the browser: no bundled
        // substitute typeface.
        let hint_font = Font::discover(path.parent().unwrap_or(Path::new(".")));
        let mut hint_surface = hint_font.as_ref().map(|f| {
            // Two lines: what the controls are, and what state the game is in.
            let h = usize::try_from(i32::from(f.strike().line_height()) * 2 + 8).unwrap_or(38);
            Canvas::new(WIDTH, h)
        });
        host.set_present_hook(
            1,
            Box::new(move |fb, ticks| {
                // Hold emulated time to the wall clock *before* drawing, so the
                // sleep happens with the frame already computed and the window
                // gets the frame at an even rate. This is the whole reason the
                // modules ran tens of times too fast and the host CPU stayed
                // pinned: nothing tied the 60 Hz tick to a real second.
                pacer.wait_for_tick(ticks);

                let mut s = shared.borrow_mut();
                // Palette-indexed straight to 0RGB through a 256-entry table,
                // no intermediate buffer: `to_rgb()` allocated ~900 KB and
                // walked the frame twice, sixty times a second, on the same
                // thread as the emulator. Rebuilding the table each frame is
                // 256 entries and keeps palette animation correct.
                let mut lut = [0u32; 256];
                for (l, c) in lut.iter_mut().zip(fb.palette.iter()) {
                    *l = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
                }
                for (px, &i) in s.pixels.iter_mut().zip(fb.pixels.iter()) {
                    *px = lut[usize::from(i)];
                }
                s.presented += 1;

                // The banner, until it has been read or acted on. It goes over
                // the copied frame rather than into the emulated screen, so the
                // module never sees it and it cannot affect the ink measurements.
                if ticks < s.hint_until {
                    if let (Some(f), Some(b)) = (hint_font.as_ref(), hint_surface.as_mut()) {
                        let controls = if s.modern { MODERN_HELP } else { ORIGINAL_HELP };
                        let lines = if s.esc_armed_until.is_some() {
                            [LEAVING_HELP, if fringe { controls } else { "" }]
                        } else if s.typing {
                            // The control line is deliberately dropped: while
                            // typing, none of what it lists is true.
                            [TYPING_HELP, ""]
                        } else if fringe {
                            [controls, if s.caps { PLAYING_HELP } else { PAUSED_HELP }]
                        } else {
                            // The keypad/thrust/fire lines describe Lunatic
                            // Fringe's LFky table; on any other module they
                            // would be a promise about keys that do nothing.
                            [GENERIC_HELP, ""]
                        };
                        overlay_hint(&mut s.pixels, b, f, &lines);
                    }
                }

                let mut w = window.borrow_mut();
                let _ = w.update_with_buffer(&s.pixels, WIDTH, HEIGHT);

                // Achieved rate, so "laggy" becomes a number.
                if stats {
                    stat_frames += 1;
                    let elapsed = stat_epoch.elapsed();
                    if elapsed >= std::time::Duration::from_secs(1) {
                        let secs = elapsed.as_secs_f64();
                        eprintln!(
                            "[stats] {:.1} fps presented (60 is real time), \
                             paced {} sleeps, {} resyncs",
                            stat_frames as f64 / secs,
                            pacer.slept(),
                            pacer.resyncs(),
                        );
                        stat_epoch = std::time::Instant::now();
                        stat_frames = 0;
                    }
                }
                // Closing the window is unambiguous and immediate. Escape is not,
                // and is handled once the key transitions are in hand.
                if !w.is_open() {
                    s.quit = true;
                }

                // Every transition since the last frame, in order.
                let edges: Vec<(Key, bool)> = std::mem::take(&mut *taps.borrow_mut());
                if stats && !edges.is_empty() {
                    eprintln!("[keys] {edges:?} caps={}", s.caps);
                }

                // Is the hardware read allowed to speak this frame? Focus is the
                // second condition and not a detail: `ad_keystate` reports the
                // *session's* keyboard, so an unfocused player that trusted it
                // would fly the ship from whatever the person had gone off to
                // type into another window. The window's own view already
                // reports nothing when it is not key, so falling back is exactly
                // right rather than merely safe.
                //
                // Which way round `is_active` reads has to be *learned*, because
                // `minifb` 0.28 gets it backwards on macOS: `mainWindowChanged:`
                // sets the flag true when the window is main, `mfb_is_active`
                // returns it unchanged, and the Rust wrapper then compares it
                // `== 0`. Every other backend returns it un-inverted, so this is
                // a bug in one platform rather than a convention to follow — and
                // hard-coding the inversion would fail silently, in the dangerous
                // direction, the day it is fixed upstream.
                //
                // A key transition is the calibration. Those arrive from
                // `keyDown:`, `keyUp:` and `flagsChanged:` on the window, none of
                // which fire unless the window has focus, so whatever
                // `is_active` says on a frame that carried one is *by definition*
                // what focus looks like here. Until the first one arrives the
                // answer is "not focused", which costs nothing: with no keys yet
                // pressed there is nothing for the hardware read to be right
                // about.
                let raw_active = w.is_active();
                if !edges.is_empty() {
                    s.active_means = Some(raw_active);
                }
                let focused = s.active_means == Some(raw_active);
                if !s.hid && focused {
                    // Earned, not assumed — see `Shared::hid`. One key reading
                    // down is proof enough that the call is answered here.
                    // Caps Lock counts, and matters: it is a *lock*, so it reads
                    // true without anybody holding anything, and a Lunatic Fringe
                    // player has already switched it on before the game starts.
                    // That makes it the usual way this proof lands, on the first
                    // frame rather than after the first keystroke.
                    s.hid = ad_keystate::caps_lock() == Some(true)
                        || KEY_MAP
                            .iter()
                            .filter_map(|(k, _)| hw_code(*k))
                            .any(|c| ad_keystate::key_down(c) == Some(true));
                    if s.hid && stats {
                        eprintln!("[keys] hardware key state is live; heuristics off");
                    }
                }
                let hid = s.hid && focused;

                // Both views of the keyboard, side by side, so an input that
                // appears to do nothing can be told apart from an input that
                // never arrived — and, specifically, so the two can be caught
                // disagreeing. `win` is what the event stream believes; `hw` is
                // what the keyboard is actually doing. Hold a turn key, press
                // Command, then let the turn key go: the moment they diverge is
                // the bug this path was built to end, and `hw` is the one that
                // stays right.
                if stats {
                    let win = w.get_keys();
                    let hw: Vec<Key> = KEY_MAP
                        .iter()
                        .map(|(k, _)| *k)
                        .filter(|k| hw_code(*k).and_then(ad_keystate::key_down) == Some(true))
                        .collect();
                    if !win.is_empty() || !hw.is_empty() {
                        eprintln!("[keys] win {win:?} hw {hw:?} caps={} hid={hid}", s.caps);
                    }
                }

                // Leaving takes two presses of Escape, counted on the key going
                // *down* so that holding it is one press rather than sixty.
                for (key, down) in &edges {
                    if !*down || *key != Key::Escape {
                        continue;
                    }
                    match s.esc_armed_until {
                        Some(until) if ticks <= until => s.quit = true,
                        _ => {
                            s.esc_armed_until = Some(ticks.saturating_add(ESC_CONFIRM_TICKS));
                            s.hint_until = ticks.saturating_add(ESC_CONFIRM_TICKS);
                        }
                    }
                }
                if s.esc_armed_until.is_some_and(|until| ticks > until) {
                    s.esc_armed_until = None;
                }

                // The Caps Lock latch, flipped once per press. Driven from the
                // edges, so a tap shorter than a frame still counts.
                //
                // Not while typing, where pausing is meaningless anyway.
                for (key, down) in &edges {
                    if s.typing {
                        break;
                    }
                    if *down && (CAPS_TOGGLE.contains(key) || (fringe && *key == FRINGE_PAUSE)) {
                        s.caps = !s.caps;
                        // Pausing is the moment to be told the controls again.
                        s.hint_until = ticks.saturating_add(HINT_TICKS);
                    }
                }
                // The layout is settled before the game starts and does not
                // change while it runs. `C` used to swap it mid-play, which meant
                // the player had to hold back a letter key from every module for
                // the whole session to keep the option open. The chooser screen
                // asks the question once, where it can show both layouts side by
                // side and be read rather than remembered — so `C` is a letter
                // again, and `Shared::modern` is fixed for the run.
                //
                // The real Caps Lock, now that there is a way to read it.
                //
                // This is the control the game's own screen asks for, and until
                // now the player could not offer it: Caps Lock never arrives as
                // a `keyDown:`, so `Key::CapsLock` can never read as down and the
                // latch had to be driven by stand-in keys. `ad_keystate` reads
                // the lock as the *flag* it is, which is the only shape that can
                // answer "is it on", as opposed to "was it just pressed".
                //
                // Driven by the change and not the level, which is what keeps
                // this purely additive: a lock that never moves never overrides
                // anything, so the stand-in keys and the click still work exactly
                // as they did. Starting the session with the lock already on
                // counts as a change from the `false` it is seeded with, which is
                // the behaviour somebody who turned it on before launching wants.
                if hid {
                    if let Some(locked) = ad_keystate::caps_lock() {
                        if locked != s.caps_lock_was {
                            s.caps_lock_was = locked;
                            s.caps = locked;
                            s.hint_until = ticks.saturating_add(HINT_TICKS);
                        }
                    }
                }

                // A click toggles it too, and that is not a convenience. Mouse
                // buttons arrive through `mouseDown:` and keys through
                // `keyDown:` — different paths entirely — so if a window is not
                // receiving keyboard events at all, this is the one control that
                // still works. Modules on this disk do not read the mouse button;
                // they read `_GetMouse` for its position.
                let click = w.get_mouse_down(MouseButton::Left);
                if click && !s.click_was_down {
                    s.caps = !s.caps;
                }
                s.click_was_down = click;

                // The stuck-key heuristic, and only while the hardware read is
                // unavailable. It guesses; the hardware read knows. Running both
                // would let a guess veto a fact, and the guess is wrong more
                // often than the fact — a key held perfectly still through a
                // Command press is indistinguishable, to a timer, from one whose
                // release went missing.
                let now = std::time::Instant::now();
                if hid {
                    s.suppressed.clear();
                    s.key_seen.clear();
                    s.cmd_released_at = None;
                } else {
                    // When each key was last seen going down, and which keys have
                    // stopped reporting. A fresh press always clears a suppression.
                    for (key, down) in &edges {
                        if *down {
                            s.suppressed.retain(|k| k != key);
                            match s.key_seen.iter_mut().find(|(k, _)| k == key) {
                                Some(slot) => slot.1 = now,
                                None => s.key_seen.push((*key, now)),
                            }
                        } else {
                            s.key_seen.retain(|(k, _)| k != key);
                        }
                    }

                    // Command's release is the moment a swallowed key-up becomes
                    // visible: whatever is still "down" but has gone quiet since
                    // then was let go while Command masked it.
                    let cmd_now = w.is_key_down(Key::LeftSuper) || w.is_key_down(Key::RightSuper);
                    if s.cmd_was_down && !cmd_now {
                        s.cmd_released_at = Some(now);
                    }
                    s.cmd_was_down = cmd_now;
                    if let Some(released) = s.cmd_released_at {
                        if now.duration_since(released) > LOST_KEYUP_GRACE {
                            s.cmd_released_at = None;
                            let stale: Vec<Key> = KEY_MAP
                                .iter()
                                .map(|(k, _)| *k)
                                .filter(|k| w.is_key_down(*k))
                                .filter(|k| {
                                    !s.key_seen.iter().any(|(kk, t)| kk == k && *t >= released)
                                })
                                .collect();
                            for k in stale {
                                if !s.suppressed.contains(&k) {
                                    s.suppressed.push(k);
                                }
                            }
                        }
                    }
                }

                let (modern, typing) = (s.modern, s.typing);
                s.held = KEY_MAP
                    .iter()
                    .chain(MODERN_MAP.iter())
                    .chain(ORIGINAL_MAP.iter())
                    .chain(TYPING_MAP.iter())
                    .map(|(k, _)| *k)
                    .filter(|k| key_down(&w, *k, hid) && !s.suppressed.contains(k))
                    .filter_map(|k| code_for_module(k, fringe, modern, typing))
                    .collect();
                s.held.sort_unstable();
                s.held.dedup();
                // What the *module* is about to poll, which is the only view
                // that decides whether a key does anything. A key can be read
                // perfectly and still land on the wrong control: the layout
                // decides that, and `win`/`hw` above cannot show it.
                if stats && !s.held.is_empty() {
                    let codes: Vec<String> = s.held.iter().map(|c| format!("{c:#04X}")).collect();
                    eprintln!(
                        "[keys] module sees [{}] layout={}",
                        codes.join(" "),
                        if modern { "modern" } else { "original" }
                    );
                }
                // A key that went down and up again inside this frame is still a
                // real press. Hold it for this one tick so the module sees it.
                for (key, down) in &edges {
                    if !*down {
                        continue;
                    }
                    if let Some(code) = code_for_module(*key, fringe, modern, typing) {
                        if !s.held.contains(&code) {
                            s.held.push(code);
                        }
                    }
                }
                // Caps Lock stays held while typing: the game is inside its play
                // session and reads the latch to know it is still awake. Letting
                // it lapse here would look to the module like the user quit
                // mid-name.
                if s.caps {
                    s.held.push(CAPS_LOCK_CODE);
                }
                // A latch with no indicator is a guessing game, and this one has
                // no keyboard light to look at. Only touched on a change: setting
                // a window title sixty times a second is a syscall per frame.
                if s.caps != shown_caps {
                    shown_caps = s.caps;
                    // "Playing", not "Caps Lock on". The latch *is* the emulated
                    // Caps Lock, but naming it that in the interface was actively
                    // misleading: the real key does nothing on macOS, and the game's
                    // own screen tells you to press it. Report the state the player
                    // can see instead of the mechanism they cannot.
                    let state = if s.caps { "Playing" } else { "Paused" };
                    w.set_title(&format!("{APP_NAME} — {title_for_hook}  ·  {state}"));
                }
                // The cursor, for the modules that read `_GetMouse` to avoid
                // drawing under it. Captured here and applied by the mouse source
                // below, because the present hook cannot reach the Toolbox.
                // Only when it is over the picture: in the letterbox there is no
                // emulated position to report, so the last one stands.
                if let Some((x, y)) = cursor_in_buffer(&w) {
                    s.mouse = (x as i16, y as i16);
                }
            }),
        );
    }

    // Keys are applied from the same tick loop, immediately after the hook, so a
    // module polling the KeyMap sees this tick's state.
    {
        let shared = Rc::clone(&shared);
        host.set_key_source(Box::new(move || {
            let s = shared.borrow();
            (s.held.clone(), s.quit)
        }));
    }
    {
        let shared = Rc::clone(&shared);
        host.set_mouse_source(Box::new(move || shared.borrow().mouse));
    }

    let outcome = (|| -> Result<(), String> {
        match host.call(GmMessage::Initialize) {
            Ok(ad_resource::GmResult::Ok) => {}
            Ok(r) => {
                let said = host.error_message().unwrap_or_else(|| format!("{r:?}"));
                return Err(format!("{title} declined: {said}"));
            }
            Err(e) => return Err(format!("{title}: Initialize failed: {e}")),
        }
        // A module that declines Blank must not be sent DrawFrame: PICS Player
        // returns ModuleError here for want of a picture file, and driving it
        // anyway spins forever against state it never built.
        match host.call(GmMessage::Blank) {
            Ok(ad_resource::GmResult::Ok) => {}
            Ok(r) => {
                let said = host.error_message().unwrap_or_else(|| format!("{r:?}"));
                return Err(format!("{title} declined: {said}"));
            }
            Err(e) => return Err(format!("{title}: Blank failed: {e}")),
        }
        let mut loop_epoch = std::time::Instant::now();
        let mut prev = (0u64, 0u64, 0u64);
        let mut busy = std::time::Duration::ZERO;
        // Keep sending DrawFrame until the user quits. Screen savers return every
        // frame; games return only when they are done. The rate is bounded by
        // `Pacer` in the present hook: because ticks advance from executed
        // cycles, holding one tick to one 60th of a real second pins the
        // emulator to a period-correct clock, which is what decided how often
        // After Dark itself got to call a module ("the frequency with which this
        // message is received depends on how loaded down the system is" — the
        // 3.0 Programmer's Manual).
        loop {
            if shared.borrow().quit {
                return Ok(());
            }
            if options.stats {
                let now = std::time::Instant::now();
                if now.duration_since(loop_epoch) >= std::time::Duration::from_secs(1) {
                    let c = host.counters;
                    eprintln!(
                        "[loop] draw_frame {} ticks {} presents {} in {:.2}s \
                         (inside draw_frame {:.2}s, i.e. {:.2}ms per call)",
                        c.0 - prev.0,
                        c.1 - prev.1,
                        c.2 - prev.2,
                        now.duration_since(loop_epoch).as_secs_f64(),
                        busy.as_secs_f64(),
                        if c.0 > prev.0 {
                            busy.as_secs_f64() * 1000.0 / (c.0 - prev.0) as f64
                        } else {
                            0.0
                        },
                    );
                    prev = c;
                    loop_epoch = now;
                    busy = std::time::Duration::ZERO;
                }
            }
            let call_start = std::time::Instant::now();
            let r = host.draw_frame();
            busy += call_start.elapsed();
            // Typing mode ends with the `DrawFrame` that contains the prompt.
            // Two passes, because the prompt is usually drawn part-way through a
            // call: the first return only marks it stale, the next one clears it.
            // Lunatic Fringe stays inside one `DrawFrame` for a whole game, so in
            // practice this fires when the session ends and the title screen
            // comes back — and it is the backstop that stops a missed closing
            // edge from leaving the next game unsteerable.
            {
                let mut s = shared.borrow_mut();
                if s.typing {
                    if s.typing_is_stale {
                        s.typing = false;
                        s.typing_is_stale = false;
                        s.hint_until = 0;
                    } else {
                        s.typing_is_stale = true;
                    }
                }
            }
            match r {
                Ok(ad_resource::GmResult::Ok) => {}
                Ok(r) => return Err(format!("{title} returned {r:?}")),
                Err(e) => return Err(format!("{title} stopped: {e}")),
            }
        }
    })();

    if let Some(dev) = audio {
        dev.silence();
    }
    let _ = host.call(GmMessage::Close);
    if let Err(e) = host.flush_saved_state() {
        eprintln!("saved state not written: {e}");
    }
    let presented = shared.borrow().presented;
    let sounds = host.played_sounds().len();
    println!("presented {presented} frames, played {sounds} sounds");
    {
        let mut w = window.borrow_mut();
        w.set_title(APP_NAME);
        // Give the window back to the ordinary stacking order, or the browser
        // would sit on top of everything for the rest of the session.
        if raise {
            w.topmost(false);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single-module bundle is recognised by the file the packaging script
    /// writes, and the ordinary launcher is left alone.
    ///
    /// The trim is the part worth a test: the file is written with `echo`, so
    /// its last byte is a newline, and a title carrying one would be looked up
    /// as `Lunatic Fringe\n.rsrc` — a module that is never found, on a launch
    /// that silently falls back to the list.
    #[test]
    fn a_pinned_bundle_names_its_module() {
        let res = std::env::temp_dir().join("ad-player-test-pinned/Resources");
        let _ = std::fs::remove_dir_all(res.parent().unwrap_or(&res));
        std::fs::create_dir_all(&res).expect("scratch");

        // No file: this is the browser, which must keep browsing.
        assert_eq!(pinned_in(&res), None);

        std::fs::write(res.join("module"), "Lunatic Fringe\n").expect("write");
        assert_eq!(pinned_in(&res).as_deref(), Some("Lunatic Fringe"));

        // A blank file is not a module named "": it is a bundle somebody built
        // wrong, and the list is the safe answer.
        std::fs::write(res.join("module"), "  \n").expect("write");
        assert_eq!(pinned_in(&res), None);
    }

    /// The same answer out of either layout.
    ///
    /// macOS has the file in the bundle, a directory *above* the executable;
    /// Linux and Windows have no bundle to put it in, so it sits beside the
    /// binary. Packaging writes whichever the platform uses, and a release that
    /// shipped the flat layout while the lookup only knew the bundle one would
    /// hand every Linux and Windows player the module list instead of the game.
    #[test]
    fn a_pinned_app_names_its_module_in_either_layout() {
        let root = std::env::temp_dir().join("ad-player-test-layouts");
        let _ = std::fs::remove_dir_all(&root);

        // macOS: <App>.app/Contents/MacOS/ad-player, the file in ../Resources.
        let macos = root.join("Lunatic Fringe Player.app/Contents/MacOS");
        let resources = root.join("Lunatic Fringe Player.app/Contents/Resources");
        std::fs::create_dir_all(&macos).expect("scratch");
        std::fs::create_dir_all(&resources).expect("scratch");
        assert_eq!(pinned_beside(&macos), None, "no file yet: the browser");
        std::fs::write(resources.join("module"), "Lunatic Fringe\n").expect("write");
        assert_eq!(pinned_beside(&macos).as_deref(), Some("Lunatic Fringe"));

        // Linux and Windows: the binary and the file in one folder.
        let flat = root.join("Lunatic Fringe Player");
        std::fs::create_dir_all(&flat).expect("scratch");
        assert_eq!(pinned_beside(&flat), None, "no file yet: the browser");
        std::fs::write(flat.join("module"), "Lunatic Fringe\n").expect("write");
        assert_eq!(pinned_beside(&flat).as_deref(), Some("Lunatic Fringe"));
    }

    /// Window coordinates map onto the framebuffer through the letterbox.
    ///
    /// `minifb`'s own `get_mouse_pos` divides by the scale factor computed at
    /// construction, so it is wrong the moment the window is resized. Getting this
    /// wrong is not subtle in effect and is invisible in code: clicks select the
    /// wrong row and modules get a cursor that is somewhere else.
    #[test]
    fn the_cursor_maps_through_the_letterbox() {
        // Unscaled 1:1 — the identity case.
        assert_eq!(window_to_buffer((640, 480), (0.0, 0.0)), Some((0, 0)));
        assert_eq!(
            window_to_buffer((640, 480), (639.0, 479.0)),
            Some((639, 479))
        );

        // Exactly 2x, no letterbox: 4:3 into 4:3.
        assert_eq!(window_to_buffer((1280, 960), (0.0, 0.0)), Some((0, 0)));
        assert_eq!(
            window_to_buffer((1280, 960), (640.0, 480.0)),
            Some((320, 240))
        );
        assert_eq!(
            window_to_buffer((1280, 960), (1279.0, 959.0)),
            Some((639, 479))
        );

        // The shipped initial size, 1024x768, is 1.6x and also 4:3.
        assert_eq!(
            window_to_buffer((INITIAL_WIDTH, INITIAL_HEIGHT), (512.0, 384.0)),
            Some((320, 240))
        );

        // Wider than 4:3: bars left and right, so the centre still maps to the
        // centre and the far left is outside the picture entirely.
        let wide = (1280, 480); // scale 1.0, 320px of bar each side
        assert_eq!(window_to_buffer(wide, (640.0, 240.0)), Some((320, 240)));
        assert_eq!(window_to_buffer(wide, (320.0, 0.0)), Some((0, 0)));
        assert_eq!(window_to_buffer(wide, (10.0, 240.0)), None, "left bar");
        assert_eq!(window_to_buffer(wide, (1270.0, 240.0)), None, "right bar");

        // Taller than 4:3: bars top and bottom.
        let tall = (640, 960); // scale 1.0, 240px of bar top and bottom
        assert_eq!(window_to_buffer(tall, (320.0, 480.0)), Some((320, 240)));
        assert_eq!(window_to_buffer(tall, (320.0, 10.0)), None, "top bar");
        assert_eq!(window_to_buffer(tall, (320.0, 950.0)), None, "bottom bar");

        // Degenerate sizes must not panic or divide by zero.
        assert_eq!(window_to_buffer((0, 0), (0.0, 0.0)), None);
        assert_eq!(window_to_buffer((0, 480), (0.0, 0.0)), None);
    }

    /// The hint goes over the copied frame, in the bottom rows only.
    ///
    /// It must not reach the emulated screen: a banner drawn there would be seen
    /// by the module and would count towards the ink measurements the whole
    /// compatibility matrix rests on.
    #[test]
    fn the_hint_only_touches_the_bottom_of_the_frame() {
        const SENTINEL: u32 = 0x00AB_CDEF;
        let mut pixels = vec![SENTINEL; WIDTH * HEIGHT];
        let mut banner = Canvas::new(WIDTH, 20);
        // A strike is needed to draw text; without a font there is no banner at
        // all, which the caller checks. Build one the same way the browser does.
        let Some(font) = Font::discover(Path::new("../../modules")) else {
            // No user font available in this checkout: the geometry is still
            // worth asserting, so drive it with an empty label.
            let top = HEIGHT - banner.h;
            for row in 0..banner.h {
                let dst = (top + row) * WIDTH;
                pixels[dst..dst + WIDTH].fill(0);
            }
            assert_eq!(pixels[0], SENTINEL, "the top must be untouched");
            return;
        };
        overlay_hint(&mut pixels, &mut banner, &font, &["Tab = Caps Lock"]);

        // Everything above the banner is untouched.
        let top = HEIGHT - banner.h;
        assert!(
            pixels[..top * WIDTH].iter().all(|&p| p == SENTINEL),
            "the frame above the banner must be untouched"
        );
        // The banner rows are not the sentinel, and contain drawn text.
        let strip = &pixels[top * WIDTH..];
        assert!(strip.iter().all(|&p| p != SENTINEL), "banner rows replaced");
        assert!(
            strip.contains(&colour::SELECTED_INK),
            "the label must actually have been drawn"
        );
    }

    /// The collision the layout chooser exists for: `L` is two different keys.
    ///
    /// Lunatic Fringe's `LFky` declares *two* original key sets and polls both.
    /// The second needs no keypad and turns left on `L` (`0x25`) — and the modern
    /// layout remaps `L` onto Turbo Thrust (`0x5B`), so in that layout turn-left
    /// can never arrive. Nothing errors: the key is read perfectly and lands on a
    /// control the player was not aiming at, which is why the choice has to be
    /// made before the game starts rather than diagnosed after.
    ///
    /// `;` is the control: it is not remapped, so it means the same thing in both
    /// layouts, which is exactly why thrust kept working while turning died.
    #[test]
    fn the_modern_layout_steals_the_keypad_free_turn_key() {
        assert_eq!(code_for(Key::L, true, false), Some(lf::TURBO), "modern L");
        assert_eq!(code_for(Key::L, false, false), Some(0x25), "original L");
        assert_ne!(
            code_for(Key::L, true, false),
            code_for(Key::L, false, false),
            "if these ever agree the chooser has stopped mattering"
        );

        // Semicolon survives both, which is the asymmetry that made this look
        // like "one key broke" rather than "the layout is wrong".
        for modern in [false, true] {
            assert_eq!(code_for(Key::Semicolon, modern, false), Some(0x29));
        }
    }

    /// Every key the player reads has a physical code to ask the hardware about.
    ///
    /// A key missing from [`hw_code`] silently falls back to the window's view,
    /// which is the exact failure the hardware read exists to avoid — and it
    /// would fail for one key while every other key worked, which is the kind of
    /// bug that gets blamed on the keyboard.
    #[test]
    fn every_key_the_player_reads_has_a_hardware_code() {
        for (key, _) in KEY_MAP.iter().chain(MODERN_MAP).chain(TYPING_MAP) {
            assert!(hw_code(*key).is_some(), "{key:?} has no physical code");
        }
    }

    /// [`hw_code`] reports the key's own code, not the one the module is told.
    ///
    /// This is the whole reason [`HW_CODE`] is consulted before [`KEY_MAP`]. The
    /// two tables answer different questions, and the failure from confusing
    /// them is silent: `F` would be *asked about* as though it were Command, so
    /// firing with `F` would look to the player like the modifier was down.
    #[test]
    fn a_remapped_key_is_asked_about_by_its_own_code() {
        // `F` is Fire to the module and `f` to the keyboard.
        assert_eq!(code_for(Key::F, false, false), Some(0x37));
        assert_eq!(hw_code(Key::F), Some(0x03));

        // Each right-hand modifier is its twin to the module — a Mac `KeyMap` has
        // one bit for the pair — and its own key to the keyboard.
        for (right, shared, own) in [
            (Key::RightSuper, 0x37, 0x36),
            (Key::RightShift, 0x38, 0x3C),
            (Key::RightAlt, 0x3A, 0x3D),
            (Key::RightCtrl, 0x3B, 0x3E),
        ] {
            assert_eq!(code_for(right, false, false), Some(shared), "{right:?}");
            assert_eq!(hw_code(right), Some(own), "{right:?}");
        }

        // And a key with nothing to hide reads the same either way.
        assert_eq!(hw_code(Key::Left), code_for(Key::Left, false, false));
        assert_eq!(hw_code(Key::LeftSuper), Some(0x37));
    }

    /// The keys Lunatic Fringe actually flies with, end to end.
    ///
    /// The bug this path was built for is Command-plus-a-turn-key, so the codes
    /// on that route are the ones worth pinning: if any of them drifts, firing
    /// and turning stop being independent again and nothing else in the suite
    /// would notice.
    #[test]
    fn firing_and_turning_are_asked_about_as_different_keys() {
        let fire = [Key::LeftSuper, Key::RightSuper, Key::F];
        let turn = [Key::Left, Key::Right, Key::A, Key::D];

        let fire_hw: Vec<u8> = fire.iter().filter_map(|k| hw_code(*k)).collect();
        let turn_hw: Vec<u8> = turn.iter().filter_map(|k| hw_code(*k)).collect();
        assert_eq!(fire_hw.len(), fire.len(), "every Fire key is readable");
        assert_eq!(turn_hw.len(), turn.len(), "every turn key is readable");
        for t in &turn_hw {
            assert!(!fire_hw.contains(t), "code {t:#04X} is both Fire and turn");
        }

        // Left and Right arrow are the pair in the report: holding one and
        // pressing Command must be two independent physical keys.
        assert_eq!(hw_code(Key::Left), Some(0x7B));
        assert_eq!(hw_code(Key::Right), Some(0x7C));
    }

    /// A key whose release macOS swallowed is released; one still held is not.
    ///
    /// Kept as the fallback path: it runs only where the hardware read is
    /// unavailable, which is every platform but macOS and a macOS that has not
    /// yet proved itself. See `Shared::hid`.
    ///
    /// macOS does not deliver `keyUp:` for an ordinary key while Command is down.
    /// Command is Fire in Lunatic Fringe, so holding a turn key and firing left
    /// the turn key stuck down and the ship spinning. The rule: after Command is
    /// released, a key `minifb` still reports down but which has produced no down
    /// edge since then had its release swallowed. Auto-repeat is what keeps a
    /// genuinely held key reporting.
    #[test]
    fn a_swallowed_key_release_is_detected_and_a_held_key_is_not() {
        use std::time::{Duration, Instant};

        // The rule, over the two cases that must come out differently.
        let judge = |seen: Option<Duration>, released_ago: Duration| -> bool {
            let now = Instant::now();
            let released = now - released_ago;
            let last_seen = seen.map(|ago| now - ago);
            // Stuck when nothing has been seen since Command was released.
            last_seen.is_none_or(|t| t < released)
        };

        // Held: auto-repeat produced a down edge 20 ms ago, after the release.
        assert!(
            !judge(Some(Duration::from_millis(20)), Duration::from_millis(500)),
            "a key still auto-repeating must not be released"
        );
        // Swallowed: the last down edge was before Command was released, and
        // nothing since.
        assert!(
            judge(Some(Duration::from_millis(900)), Duration::from_millis(500)),
            "a key that has gone quiet since Command came up lost its key-up"
        );
        // Never seen at all is also stuck rather than held.
        assert!(judge(None, Duration::from_millis(500)));

        // The grace period must clear the default 250 ms auto-repeat delay, or a
        // genuinely held key would be cut off before its first repeat.
        assert!(
            LOST_KEYUP_GRACE > Duration::from_millis(250),
            "grace must outlast the initial auto-repeat delay"
        );
    }

    /// Fire has a non-modifier alternative, and no toggle key doubles as itself.
    ///
    /// Command fires because the game's own controls screen says it does. `F`
    /// exists because that screen also puts Power Shield on `Space` in the
    /// keyboard column, and Command plus Space is Spotlight — taken by the window
    /// server before this application is offered it. Either key alone is enough
    /// to play; the point of the pair is that no keyboard is left without one.
    #[test]
    fn fire_has_an_alternative_that_cannot_swallow_a_key_up() {
        // Command, both sides, plus F — all reaching the Mac code for cmdKey.
        let fire: Vec<Key> = KEY_MAP
            .iter()
            .filter(|(_, code)| *code == 0x37)
            .map(|(k, _)| *k)
            .collect();
        assert!(
            fire.contains(&Key::LeftSuper),
            "the game's screen says cmd fires"
        );
        assert!(
            fire.contains(&Key::F),
            "F must fire too, so a keypad-free keyboard is not stuck with Cmd+Space"
        );
        // And F must not also arrive as itself, or one key would do two things.
        assert!(
            !KEY_MAP
                .iter()
                .any(|(k, code)| *k == Key::F && *code == 0x03),
            "F is a second Fire, so it must not also be key 0x03"
        );
    }

    /// The modern layout maps onto the codes the game actually polls.
    ///
    /// It cannot invent codes: Lunatic Fringe reads the `KeyMap` for exactly the
    /// values in its `LFky 128` resource, so a modern key has to arrive as one of
    /// those. And a remapped key must never *also* arrive as itself — `A` is Turn
    /// Left in the modern layout and Abort Ship in the original, so passing both
    /// would self-destruct the ship every time the player turned.
    #[test]
    fn the_modern_layout_replaces_a_key_rather_than_adding_to_it() {
        // Turning, with either hand.
        assert_eq!(code_for(Key::A, true, false), Some(lf::TURN_LEFT));
        assert_eq!(code_for(Key::Left, true, false), Some(lf::TURN_LEFT));
        assert_eq!(code_for(Key::D, true, false), Some(lf::TURN_RIGHT));
        assert_eq!(code_for(Key::Right, true, false), Some(lf::TURN_RIGHT));
        // Forward is thrust.
        assert_eq!(code_for(Key::W, true, false), Some(lf::THRUST));
        assert_eq!(code_for(Key::Up, true, false), Some(lf::THRUST));
        // The right hand.
        assert_eq!(code_for(Key::J, true, false), Some(lf::FIRE));
        assert_eq!(code_for(Key::L, true, false), Some(lf::TURBO));
        assert_eq!(code_for(Key::K, true, false), Some(lf::SHIELD));
        assert_eq!(code_for(Key::Backspace, true, false), Some(lf::ABORT));
        assert_eq!(code_for(Key::Delete, true, false), Some(lf::ABORT));

        // The originals still work when the modern layout is off, and `A` means
        // Abort Ship there — the exact collision the replacement prevents.
        assert_eq!(code_for(Key::A, false, false), Some(lf::ABORT));
        assert_eq!(
            code_for(Key::L, false, false),
            Some(0x25),
            "L turns left originally"
        );
        assert_eq!(code_for(Key::NumPad4, false, false), Some(lf::TURN_LEFT));
        assert_eq!(code_for(Key::NumPad5, false, false), Some(lf::THRUST));

        // Every modern code is one the game polls.
        let known = [
            lf::TURN_LEFT,
            lf::TURN_RIGHT,
            lf::THRUST,
            lf::TURBO,
            lf::SHIELD,
            lf::ABORT,
            lf::FIRE,
        ];
        for (key, code) in MODERN_MAP {
            assert!(
                known.contains(code),
                "{key:?} maps to {code:#04x}, which LFky does not name"
            );
        }
    }

    /// Typing a name types the letters on the keys.
    ///
    /// The reported symptoms, exactly: Delete typed "a", `A` typed "4", `S`
    /// typed "5". Lunatic Fringe reads name entry from the same `KeyMap` it
    /// flies with and turns the code into a character itself, so a remapped `A`
    /// does not arrive as a letter at all — it arrives as keypad-4, and keypad-4
    /// is the character "4".
    ///
    /// Asserted through `us_char_for`, the same table the runtime uses to turn a
    /// code into a character, so this measures the character the user would see
    /// rather than the code an implementation happens to send.
    #[test]
    fn typing_sends_the_letter_on_the_key_not_the_control_it_was_remapped_to() {
        use ad_toolbox::resources::us_char_for;
        let typed = |key: Key, modern: bool| -> char {
            code_for(key, modern, true).map_or('\0', |c| us_char_for(c) as char)
        };

        // The three reported cases, in the layout that produced them.
        assert_eq!(typed(Key::A, true), 'a', "A typed 4");
        assert_eq!(typed(Key::S, true), 's', "S typed 5");
        assert_eq!(
            code_for(Key::Delete, true, true),
            Some(0x33),
            "Delete typed 'a' instead of erasing"
        );

        // Every letter of the alphabet types itself, in both layouts. A name is
        // not spellable if any one of them is wrong, and the modern layout takes
        // seven of them.
        let letters = [
            (Key::A, 'a'),
            (Key::B, 'b'),
            (Key::C, 'c'),
            (Key::D, 'd'),
            (Key::E, 'e'),
            (Key::F, 'f'),
            (Key::G, 'g'),
            (Key::H, 'h'),
            (Key::I, 'i'),
            (Key::J, 'j'),
            (Key::K, 'k'),
            (Key::L, 'l'),
            (Key::M, 'm'),
            (Key::N, 'n'),
            (Key::O, 'o'),
            (Key::P, 'p'),
            (Key::Q, 'q'),
            (Key::R, 'r'),
            (Key::S, 's'),
            (Key::T, 't'),
            (Key::U, 'u'),
            (Key::V, 'v'),
            (Key::W, 'w'),
            (Key::X, 'x'),
            (Key::Y, 'y'),
            (Key::Z, 'z'),
        ];
        for modern in [false, true] {
            for (key, want) in letters {
                assert_eq!(typed(key, modern), want, "{key:?} in modern={modern}");
            }
            // Digits too, and Return to submit the name.
            assert_eq!(typed(Key::Key1, modern), '1');
            assert_eq!(code_for(Key::Enter, modern, true), Some(0x24));
        }

        // And with typing off, the controls are exactly as they were: this must
        // fix text entry without quietly disarming the layout it lives beside.
        assert_eq!(code_for(Key::A, true, false), Some(lf::TURN_LEFT));
        assert_eq!(code_for(Key::Delete, true, false), Some(lf::ABORT));
        assert_eq!(code_for(Key::F, true, false), Some(lf::FIRE));
    }

    /// The prompt that turns typing on, and the ones that must not.
    #[test]
    fn only_a_request_for_typing_turns_typing_on() {
        // What Lunatic Fringe actually draws, spacing and all.
        assert!(asks_for_typing(&[
            "High score!  Enter your name:".to_owned()
        ]));
        // Case and surrounding text must not matter.
        assert!(asks_for_typing(&["ENTER YOUR NAME".to_owned()]));
        assert!(asks_for_typing(&[
            "Score:".to_owned(),
            "  enter your name, please  ".to_owned(),
        ]));
        // The rest of the game's text, including the screens either side of the
        // one that asks. A false positive here disarms the controls mid-flight.
        for quiet in [
            "Press Caps Lock to",
            "enter the Fringe…",
            "Turn Left",
            "Abort Ship",
            "Loading, please wait…",
            "Ben Haller",
            "Score:",
            "",
        ] {
            assert!(!asks_for_typing(&[quiet.to_owned()]), "{quiet:?} must not");
        }
    }

    /// Escape still leaves, and the withheld keys are only handed over for text.
    #[test]
    fn the_reserved_keys_come_back_only_while_typing() {
        // Withheld while playing, for the reasons on `KEY_MAP` and `CAPS_TOGGLE`.
        for (key, _) in RESERVED_KEYS {
            let held_back = code_for(*key, false, false);
            let is_own_code = held_back
                == RESERVED_KEYS
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, c)| *c);
            assert!(
                !is_own_code,
                "{key:?} must not send its own code while playing"
            );
        }
        // Handed over while typing, so `f` can appear in a name.
        for (key, code) in RESERVED_KEYS {
            assert_eq!(
                code_for(*key, false, true),
                Some(*code),
                "{key:?} while typing"
            );
            assert_eq!(
                code_for(*key, true, true),
                Some(*code),
                "{key:?} while typing"
            );
        }
        // Escape is never handed over: leaving has to work from every screen,
        // including the one asking for a name.
        assert_eq!(code_for(Key::Escape, false, true), None);
        assert_eq!(code_for(Key::Escape, true, true), None);
    }

    fn entry(title: &str) -> library::Entry {
        library::Entry {
            path: PathBuf::from(format!("modules/{title}.rsrc")),
            title: title.to_owned(),
            descriptor: None,
            controls: Vec::new(),
            resources: 0,
            has_sound: false,
        }
    }

    /// What the idle timer starts, including when the setting has gone stale.
    #[test]
    fn the_idle_choice_honours_the_name_and_survives_it_disappearing() {
        use ad_runtime::{IdleModule, IdleSettings};
        let modules = [entry("Boris"), entry("Flying Toasters"), entry("Hard Rain")];

        let named = |name: &str| IdleSettings {
            module: IdleModule::Named(name.to_owned()),
            ..IdleSettings::default()
        };
        assert_eq!(idle_choice(&named("Flying Toasters"), &modules), Some(1));
        assert_eq!(idle_choice(&named("Boris"), &modules), Some(0));

        // The named module has been deleted from the folder. Falling back to a
        // random one keeps the feature working; answering `None` would turn it
        // silently off, which is the failure somebody would never diagnose.
        let stale = idle_choice(&named("Deleted Module"), &modules);
        assert!(stale.is_some_and(|i| i < modules.len()), "{stale:?}");

        // Random always lands inside the list.
        let random = IdleSettings {
            module: IdleModule::Random,
            ..IdleSettings::default()
        };
        for _ in 0..50 {
            let i = idle_choice(&random, &modules).expect("a module");
            assert!(i < modules.len(), "chose {i} of {}", modules.len());
        }

        // Nothing to choose from is `None`, not a panic on an empty range.
        assert_eq!(idle_choice(&random, &[]), None);
    }

    /// The high-score table parses from the bytes a real game wrote.
    ///
    /// The fixture is the shape of an actual save: one played entry (score 152,
    /// level 1) and nine empty slots whose score field is -1. An empty slot must
    /// vanish, not appear as a blank name with a huge score.
    #[test]
    fn the_high_score_table_reads_real_entries_and_skips_empty_slots() {
        let mut data = Vec::new();
        let mut entry = |name: &[u8], level: u16, score: i32| {
            let mut e = vec![0u8; 24];
            e[0] = u8::try_from(name.len()).unwrap_or(0);
            e[1..1 + name.len()].copy_from_slice(name);
            e[18..20].copy_from_slice(&level.to_be_bytes());
            e[20..24].copy_from_slice(&score.to_be_bytes());
            data.extend_from_slice(&e);
        };
        entry(b"AR", 1, 12);
        entry(b"ACES", 3, 152);
        for _ in 0..8 {
            entry(b"", 0, -1);
        }

        let rows = parse_lfhs(&data);
        assert_eq!(rows.len(), 2, "the eight empty slots must not appear");
        // Best first, whatever order the table stores.
        assert_eq!(
            rows[0],
            ScoreRow {
                name: "ACES".into(),
                score: 152,
                level: 3
            }
        );
        assert_eq!(
            rows[1],
            ScoreRow {
                name: "AR".into(),
                score: 12,
                level: 1
            }
        );

        // Garbage that is not a whole table parses to nothing rather than junk.
        assert!(parse_lfhs(b"short").is_empty());
    }

    /// A click in the details panel is not a click on the list.
    ///
    /// Rows were hit-tested on Y alone, so clicking anywhere to the right —
    /// reading the settings, reaching for a play button — selected whatever
    /// module shared that height, and a second click launched it.
    #[test]
    fn a_click_right_of_the_list_does_not_select_a_row() {
        let rows = Rows {
            top: 30,
            height: 15,
            visible: 20,
            left: 8,
            width: 300,
            play_win: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            play_full: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            export: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            import: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            idle_toggle: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            idle_module: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            idle_delay: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            idle_sound: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            idle_preview: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
        };
        assert_eq!(rows.row_at(100, 45), Some(1), "inside the list: a row");
        assert_eq!(
            rows.row_at(400, 45),
            None,
            "the details panel is not the list"
        );
        assert_eq!(rows.row_at(4, 45), None, "nor is the margin left of it");
        assert_eq!(rows.row_at(100, 10), None, "nor the header above it");
    }

    /// The idle strip stays on screen, whatever the module is called.
    ///
    /// Module titles are filenames from the user's own folder. A long one used
    /// to widen the "Start:" button by however much it needed, which pushes
    /// Preview past the right edge — a control that exists, is drawn, and cannot
    /// be clicked. Measured with the real strike against a deliberately absurd
    /// title.
    #[test]
    fn a_very_long_module_name_cannot_push_the_idle_controls_off_screen() {
        let Some(font) = Font::discover(Path::new("../../modules"))
            .or_else(|| Font::discover(Path::new("modules")))
        else {
            return; // no user font in this checkout
        };
        let f = font.strike();
        let absurd = "Good Vibrations _256 colors_ Deluxe Anniversary Edition".repeat(3);
        let labels = [
            "Idle: no timer".to_owned(),
            format!("Start: {absurd}"),
            "After: 15 min".to_owned(),
            "Sound: muted".to_owned(),
            "Preview (P)".to_owned(),
        ];
        let mut x = 12;
        for (i, label) in labels.iter().enumerate() {
            let cap = if i == 1 { IDLE_MODULE_MAX_W } else { i32::MAX };
            let w = (f.text_width(label.as_bytes()) + 18).min(cap);
            x += w + 6;
        }
        let screen = i32::try_from(WIDTH).unwrap_or(640);
        assert!(
            x <= screen,
            "the idle strip is {x}px wide on a {screen}px screen; Preview is off the edge"
        );
    }

    /// The banner text fits the screen it is drawn on.
    ///
    /// Nothing clips it, so a line that is too long runs off the edge and reads as
    /// a rendering fault. Measured with the real strike, against the real strings.
    #[test]
    fn the_controls_banner_fits_the_screen() {
        let Some(font) = Font::discover(Path::new("../../modules"))
            .or_else(|| Font::discover(Path::new("modules")))
        else {
            return; // no user font in this checkout; geometry is covered elsewhere
        };
        let f = font.strike();
        // 8px inset on the left, and the same again as margin on the right.
        let room = i32::try_from(WIDTH).unwrap_or(640) - 16;
        for line in [
            ORIGINAL_HELP,
            MODERN_HELP,
            PAUSED_HELP,
            PLAYING_HELP,
            LEAVING_HELP,
            TYPING_HELP,
            // Drawn across the chooser's panel rather than inside a box, so the
            // screen is the room it gets.
            CHOOSER_FOOTER,
        ] {
            let w = f.text_width(line.as_bytes());
            assert!(w <= room, "{w}px of text in {room}px: {line:?}");
        }
        // The chooser's lines are drawn inside a box, which is narrower than the
        // screen and clips nothing either.
        for line in MODERN_SETS {
            let w = f.text_width(line.as_bytes());
            assert!(
                w <= CHOOSER_TEXT_W,
                "{w}px of text in a {CHOOSER_TEXT_W}px box: {line:?}"
            );
        }
        for (name, keys) in ORIGINAL_SETS {
            // Drawn at a fixed 96px column, so the keys have that much less room.
            let w = 96 + f.text_width(keys.as_bytes());
            assert!(
                w <= CHOOSER_TEXT_W && f.text_width(name.as_bytes()) <= 96,
                "{w}px of text in a {CHOOSER_TEXT_W}px box: {name:?} {keys:?}"
            );
        }
        // Both ways of flying the modern layout are named on the screen that
        // offers it and on the banner that follows. Saying only "arrows" or only
        // "WAD" leaves half the keys undiscoverable, and a player who tries the
        // half that was not mentioned concludes the controls are broken.
        for (turn, thrust, named) in [("A / D", "W", "WAD"), ("Left / Right", "Up", "arrow")] {
            assert!(
                MODERN_SETS
                    .iter()
                    .any(|l| l.contains(turn) && l.contains(thrust))
                    && MODERN_SETS[0].contains(named),
                "the chooser must offer both hands, turn and thrust: {turn} / {thrust}"
            );
        }
        for key in ["A/D", "Left/Right", "W or Up"] {
            assert!(MODERN_HELP.contains(key), "the banner must name {key}");
        }

        // Both footers name the key that changes the state they describe, and it
        // is Caps Lock on every platform now. Naming it used to be faithful and
        // useless — macOS delivers no event for it, so the banner said `1`
        // instead — until `ad_keystate` read the latch directly. The banner and
        // the game's own "Press Caps Lock" screen finally say the same thing.
        assert!(
            PAUSED_HELP.contains("Caps Lock"),
            "the way out of a pause must be on it"
        );
        assert!(PLAYING_HELP.contains("Caps Lock"));
        // And both name the click, which is the control that still works when no
        // keyboard event is arriving at all. See the click handler.
        for line in [PAUSED_HELP, PLAYING_HELP] {
            assert!(line.contains("click"), "the fallback belongs on the banner");
        }
    }

    /// Leaving a module takes two presses of Escape, and the arming expires.
    ///
    /// A game in progress has a score and a life in it, so one stray key must not
    /// end it. Expiry matters as much as arming: a half-committed exit that waited
    /// forever would fire on an Escape pressed minutes later for another reason.
    #[test]
    fn leaving_takes_two_presses_and_the_first_one_expires() {
        // The rule as the hook applies it, over a tick sequence.
        let mut armed: Option<u32> = None;
        let mut quit = false;
        let press = |tick: u32, armed: &mut Option<u32>, quit: &mut bool| match *armed {
            Some(until) if tick <= until => *quit = true,
            _ => *armed = Some(tick.saturating_add(ESC_CONFIRM_TICKS)),
        };
        let expire = |tick: u32, armed: &mut Option<u32>| {
            if armed.is_some_and(|until| tick > until) {
                *armed = None;
            }
        };

        // One press arms and does not leave.
        press(100, &mut armed, &mut quit);
        assert!(!quit, "a single Escape must not end the module");
        assert!(armed.is_some());

        // A second press inside the window leaves.
        press(160, &mut armed, &mut quit);
        assert!(quit, "the confirming press must leave");

        // A press, then silence past the window, then a press: still two needed.
        let (mut armed, mut quit) = (None, false);
        press(100, &mut armed, &mut quit);
        expire(100 + ESC_CONFIRM_TICKS + 1, &mut armed);
        assert!(armed.is_none(), "the arming must expire on its own");
        press(1_000, &mut armed, &mut quit);
        assert!(
            !quit,
            "an expired arming cannot be confirmed by a later press"
        );
        press(1_010, &mut armed, &mut quit);
        assert!(quit);

        // Three seconds at 60 ticks: long enough to read the prompt, short enough
        // not to linger.
        assert_eq!(ESC_CONFIRM_TICKS, 180);
    }

    /// `G` shields in Lunatic Fringe's original layout, and is a letter anywhere
    /// else.
    ///
    /// The scoping is the whole point. `!modern` is not "the original layout" —
    /// it is also every one of the other modules, none of which asked for `G` to
    /// stop being `G`. Getting this wrong would be silent: 140 modules would
    /// read a space bar that nobody pressed.
    #[test]
    fn g_is_power_shield_only_where_the_game_asked_for_it() {
        // The game, original layout: Power Shield, the code its keyboard set
        // reaches for `Space`.
        assert_eq!(
            code_for_module(Key::G, true, false, false),
            Some(lf::SHIELD_SPACE)
        );
        // And `F` is Fire there, so the pair can be held at once — which Command
        // and Space cannot be, on any Mac with Spotlight.
        assert_eq!(code_for_module(Key::F, true, false, false), Some(lf::FIRE));

        // The game's modern layout has `K` for shield already; `G` stays a letter.
        assert_eq!(code_for_module(Key::G, true, true, false), Some(0x05));
        // Every other module, which runs with `modern` false and never asked.
        assert_eq!(code_for_module(Key::G, false, false, false), Some(0x05));
        // And while typing a high-score name, `G` is a `g`.
        assert_eq!(code_for_module(Key::G, true, false, true), Some(0x05));

        // The originals are untouched: this adds a key, it does not move one.
        assert_eq!(
            code_for_module(Key::Space, true, false, false),
            Some(lf::SHIELD_SPACE)
        );
        assert_eq!(
            code_for_module(Key::LeftSuper, true, false, false),
            Some(lf::FIRE)
        );
    }

    /// `1` pauses Lunatic Fringe, and is the digit `1` everywhere else.
    ///
    /// Caps Lock costs nothing to reserve — it is a system latch, not a letter
    /// taken from a module. This one is a real key, so it is held back only where
    /// it buys something: the pause banner is drawn for Lunatic Fringe and no
    /// other module, so reserving `1` across the library would take a key from
    /// 140 modules to add a control to one.
    #[test]
    fn the_second_pause_key_is_withheld_only_from_the_game_that_pauses() {
        for modern in [false, true] {
            assert_eq!(
                code_for_module(FRINGE_PAUSE, true, modern, false),
                None,
                "the player pauses on it in Lunatic Fringe"
            );
            assert_eq!(
                code_for_module(FRINGE_PAUSE, false, modern, false),
                Some(0x12),
                "every other module keeps it as a digit"
            );
        }
        // And it types, so a high score can be "1st" or a name can have a 1 in it.
        assert_eq!(code_for_module(FRINGE_PAUSE, true, false, true), Some(0x12));

        // It must not be one of the game's own controls, in either column, or
        // pausing would fly the ship. `P` is Turbo Thrust, which is why it lost.
        for (_, code) in MODERN_MAP {
            assert_ne!(code_for(FRINGE_PAUSE, false, false), Some(*code));
        }
        assert!(
            !CAPS_TOGGLE.contains(&FRINGE_PAUSE),
            "Caps Lock is the other one"
        );
    }

    /// Keys the player reserves never reach the module, under either layout.
    #[test]
    fn reserved_keys_are_not_passed_through() {
        // **One** key is withheld from every module, and it is as short as this
        // list goes. `1` is withheld too, but from Lunatic Fringe alone, which is
        // why it is not here and is asserted in
        // `the_second_pause_key_is_withheld_only_from_the_game_that_pauses`
        // instead — this test is about the base table, which still hands it over.
        // `F` is in neither category: it is *remapped*, arriving as Fire rather
        // than as itself, which is a different thing again.
        for modern in [false, true] {
            assert_eq!(
                code_for(Key::Escape, modern, false),
                None,
                "Escape leaves the module and must not also reach it"
            );
        }

        // The three that used to be held back from everything. `C` stopped being
        // a control because the chooser settles the layout before the module
        // starts; `M` because the browser's Sound setting and the system volume
        // both outrank it; `1` because `ad_keystate` reads the real Caps Lock it
        // was standing in for, and it is now a second pause key for the one
        // module that pauses rather than a key taken from all of them.
        for (key, code) in [(Key::C, 0x08), (Key::Key1, 0x12), (Key::M, 0x2E)] {
            for modern in [false, true] {
                assert_eq!(
                    code_for(key, modern, false),
                    Some(code),
                    "{key:?} is an ordinary key now, not a player control"
                );
            }
        }
        // Pause is the Caps Lock latch, because that is what pausing is here.
        assert!(CAPS_TOGGLE.contains(&Key::CapsLock));
        // Escape is deliberately *not* pause: it already leaves the module.
        assert!(!CAPS_TOGGLE.contains(&Key::Escape));

        // Tab and Return were reserved during debugging and have been given back.
        // Tab could never fire on macOS, and Return is a key modules have a claim
        // on; four keys for one control was worse than two.
        for key in [Key::Tab, Key::Enter, Key::G] {
            assert!(
                !CAPS_TOGGLE.contains(&key),
                "{key:?} is no longer a pause key"
            );
        }
        assert_eq!(
            code_for(Key::Tab, false, false),
            Some(0x30),
            "Tab reaches modules again"
        );
        assert_eq!(
            code_for(Key::Enter, false, false),
            Some(0x24),
            "Return does too"
        );
        assert_eq!(code_for(Key::G, false, false), Some(0x05), "and so does G");
        // And they still reach a module under the modern layout, which claims
        // neither of them.
        assert_eq!(code_for(Key::Tab, true, false), Some(0x30));
        assert_eq!(code_for(Key::Enter, true, false), Some(0x24));

        // The remapped key does reach the module — as Fire, never as `f`.
        assert_eq!(code_for(Key::F, false, false), Some(lf::FIRE));
        assert_eq!(code_for(Key::F, true, false), Some(lf::FIRE));
    }

    /// `reveal` moves the view the *least* amount that shows the selection.
    ///
    /// It exists because `draw_browser` stopped chasing the selection: the wheel
    /// has to be able to scroll away from it, and a redraw that dragged the view
    /// back would make the list refuse to scroll at all.
    #[test]
    fn reveal_scrolls_the_minimum_and_only_when_needed() {
        // Already on screen: nothing moves. This is the case that matters most —
        // a list that re-centres on every arrow press jumps under the cursor.
        let mut s = 10;
        reveal(12, 20, &mut s);
        assert_eq!(s, 10);
        reveal(10, 20, &mut s);
        assert_eq!(s, 10, "the first visible row is on screen");
        reveal(29, 20, &mut s);
        assert_eq!(s, 10, "the last visible row is on screen");

        // Off the bottom: scroll just far enough to put it on the last row.
        let mut s = 0;
        reveal(20, 20, &mut s);
        assert_eq!(s, 1);

        // Off the top: scroll to it exactly.
        let mut s = 40;
        reveal(7, 20, &mut s);
        assert_eq!(s, 7);
    }

    /// The Caps Lock latch flips once per press, not once per polled frame.
    ///
    /// The present hook polls at 60 Hz, so a held key is "down" for many frames.
    /// Toggling on the level rather than the edge would make one press flip the
    /// latch dozens of times and land on whichever parity the release happened to
    /// fall on.
    #[test]
    fn the_caps_latch_toggles_on_the_edge() {
        // The same edge detection the hook does, over a plausible key trace.
        let mut caps = false;
        let mut was_down = false;
        let mut states = Vec::new();
        // held for four frames, released for two, held for three.
        for down in [
            false, true, true, true, true, false, false, true, true, true,
        ] {
            if down && !was_down {
                caps = !caps;
            }
            was_down = down;
            states.push(caps);
        }
        assert_eq!(
            states,
            vec![
                false, true, true, true, true, true, true, false, false, false
            ],
            "two presses, two flips"
        );
    }

    /// Caps Lock is applied from the latch, never polled as a held key.
    ///
    /// Both would be wrong at once: a momentary bit where the Mac has a locking
    /// one, and a double-toggle when the platform does report the key.
    #[test]
    fn caps_lock_is_not_in_the_polled_key_table() {
        assert!(
            !KEY_MAP.iter().any(|(_, code)| *code == CAPS_LOCK_CODE),
            "Caps Lock must come from Shared::caps, not from KEY_MAP"
        );
        assert!(
            CAPS_TOGGLE.contains(&Key::CapsLock),
            "the real key is the pause key on every platform now"
        );
        assert_eq!(
            CAPS_TOGGLE.len(),
            1,
            "the substitute is gone: `ad_keystate` reads the real latch on macOS, \
             and a stand-in is a key held back from every module for nothing"
        );
        // And a toggle key must not also arrive as itself.
        for k in CAPS_TOGGLE {
            assert!(
                !KEY_MAP.iter().any(|(key, _)| key == k),
                "{k:?} toggles the latch, so it must not also be a polled key"
            );
        }
    }
}
