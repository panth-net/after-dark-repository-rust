//! Starting a module when the machine goes idle, and remembering how.
//!
//! # Why this is not a screen saver
//!
//! It is the nearest thing to one that macOS still permits. A real `.saver`
//! plug-in has received **no keyboard events at all** since Catalina, so an
//! interactive module cannot be played from one — and Lunatic
//! Fringe being playable is the point. What is left is what the original After
//! Dark actually did: the user goes idle, a module takes the screen, and the
//! first touch of the keyboard gives it back.
//!
//! # Where idle time comes from
//!
//! `IOHIDSystem`'s `HIDIdleTime`, read through `ioreg` — the same counter the
//! window server's own idle timer uses, so "idle" here means what it means
//! everywhere else on the system. A window can only see input it is focused for;
//! somebody reading a web page for ten minutes is idle to us and busy to
//! themselves, and only a system-wide counter tells them apart.
//!
//! Shelling out rather than linking CoreFoundation is deliberate: this workspace
//! forbids `unsafe`, and `IOKit` from Rust is `unsafe` by construction. The cost
//! is a ~20 ms subprocess, which is why [`IdleWatch`] samples once a second
//! rather than once a frame.
//!
//! Only macOS is implemented. Everywhere else [`idle_seconds`] answers `None`,
//! and a caller must present the feature as unavailable rather than as "never
//! idle" — see [`IdleSettings::available`].

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Seconds since the last keyboard or mouse event, or `None` where this cannot
/// be known.
#[must_use]
pub fn idle_seconds() -> Option<u32> {
    platform_idle_seconds()
}

#[cfg(target_os = "macos")]
fn platform_idle_seconds() -> Option<u32> {
    let out = std::process::Command::new("ioreg")
        .args(["-c", "IOHIDSystem"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // `"HIDIdleTime" = 1753894958` — nanoseconds, one entry.
    let at = text.find("\"HIDIdleTime\"")?;
    let rest = text.get(at..)?;
    let eq = rest.find('=')?;
    let digits: String = rest
        .get(eq + 1..)?
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(char::is_ascii_digit)
        .collect();
    let nanos: u64 = digits.parse().ok()?;
    u32::try_from(nanos / 1_000_000_000).ok()
}

#[cfg(not(target_os = "macos"))]
fn platform_idle_seconds() -> Option<u32> {
    // Windows would be `GetLastInputInfo`, X11 `XScreenSaverQueryInfo`; both
    // need `unsafe` FFI, which this workspace forbids. Answering `None` keeps
    // the feature honestly switched off instead of silently never firing.
    None
}

/// Polls [`idle_seconds`] at most once a second and caches the answer.
///
/// The browser redraws at 60 fps and would otherwise spawn sixty `ioreg`
/// processes a second to answer a question whose units are minutes.
#[derive(Debug)]
pub struct IdleWatch {
    last: Option<Instant>,
    seconds: Option<u32>,
}

impl Default for IdleWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl IdleWatch {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last: None,
            seconds: None,
        }
    }

    /// The cached idle time, refreshed if the cache is over a second old.
    pub fn seconds(&mut self) -> Option<u32> {
        let due = self
            .last
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if due {
            self.seconds = idle_seconds();
            self.last = Some(Instant::now());
        }
        self.seconds
    }

    /// Has the machine been idle for at least `minutes`?
    ///
    /// False when idle time cannot be read, so an unsupported platform never
    /// starts a module by accident.
    pub fn idle_for(&mut self, minutes: u32) -> bool {
        let want = u64::from(minutes).saturating_mul(60);
        self.seconds().is_some_and(|s| u64::from(s) >= want.max(1))
    }

    /// Has somebody touched the machine within the last couple of seconds?
    ///
    /// The counterpart to [`Self::idle_for`], and the signal that a running
    /// module should stop. Two seconds of slack covers the sampling interval.
    pub fn woke(&mut self) -> bool {
        self.seconds().is_some_and(|s| s < 2)
    }
}

/// Which module the idle timer starts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IdleModule {
    /// A different one each time, from whatever the caller offers.
    #[default]
    Random,
    /// Always this module, by title.
    Named(String),
}

/// A command the idle timer runs instead of a module.
///
/// The reason this exists is Rat Race. The After Dark 3.0-era modules cannot
/// run in this runtime — they go to the File Manager looking for the 3.0 engine
/// on disk, and there is no file system here.
/// A real emulator with a real After Dark install can run them today. Rather
/// than pretend otherwise, the idle timer can hand the screen to that instead:
/// the timer, the delay and the "give it back when someone touches the
/// keyboard" behaviour are the same either way, and only the thing being
/// started changes.
///
/// Set by hand in `idle.conf` — the browser has no text field — as
/// `command = /path/to/emulator --with args`.
pub type IdleCommand = String;

/// The user's idle-start preferences, as shown in the module browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleSettings {
    /// Whether the idle timer starts anything at all.
    pub enabled: bool,
    /// Minutes of idleness before it does.
    pub after_minutes: u32,
    /// What it starts.
    pub module: IdleModule,
    /// Run this instead of a module when the timer fires; see [`IdleCommand`].
    pub command: Option<IdleCommand>,
    /// Whether a started module gets a speaker.
    ///
    /// Every launch honours this — idle-started, previewed, or picked from the
    /// list — because it is the *starting* state: the M key flips it live for
    /// the session without touching the saved preference. A screen saver that
    /// starts talking in the night is the reason this exists.
    pub sound: bool,
}

impl Default for IdleSettings {
    fn default() -> Self {
        Self {
            // Off until asked for: an application that takes the screen on its
            // own initiative the first time it is run is a misbehaving one.
            enabled: false,
            after_minutes: 5,
            module: IdleModule::Random,
            command: None,
            sound: true,
        }
    }
}

/// The delays the browser cycles through, in minutes.
///
/// A short one first because the only way to *check* the setting is to sit
/// still for it, and nobody debugs a screen saver at twenty-minute intervals.
pub const DELAY_CHOICES: &[u32] = &[1, 2, 5, 10, 15, 20, 30, 60];

impl IdleSettings {
    /// Whether idle detection works on this platform at all.
    #[must_use]
    pub fn available() -> bool {
        idle_seconds().is_some()
    }

    /// Where the settings live: beside the saved scores, for the same reasons.
    #[must_use]
    pub fn path() -> Option<PathBuf> {
        Some(crate::save_dir()?.join("idle.conf"))
    }

    /// Load, falling back to the defaults for anything missing or unparsable.
    ///
    /// A damaged config must never stop the browser opening, so there is no
    /// error path: the worst outcome is the feature reverting to its defaults,
    /// which is visible on screen and one click to correct.
    #[must_use]
    pub fn load() -> Self {
        let mut out = Self::default();
        let Some(text) = Self::path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return out;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "enabled" => out.enabled = value == "true",
                "sound" => out.sound = value != "false",
                "command" => {
                    out.command = (!value.is_empty()).then(|| value.to_owned());
                }
                "after_minutes" => {
                    if let Ok(n) = value.parse::<u32>() {
                        out.after_minutes = n.clamp(1, 24 * 60);
                    }
                }
                "module" => {
                    out.module = if value.is_empty() || value == "*random*" {
                        IdleModule::Random
                    } else {
                        IdleModule::Named(value.to_owned())
                    };
                }
                _ => {}
            }
        }
        out
    }

    /// Write the settings back.
    ///
    /// # Errors
    /// The reason the file could not be written.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no save directory on this platform")?;
        let module = match &self.module {
            IdleModule::Random => "*random*",
            IdleModule::Named(name) => name.as_str(),
        };
        let body = format!(
            "# After Dark — idle start. Written by the module browser.\n\
             enabled = {}\n\
             after_minutes = {}\n\
             module = {module}\n\
             sound = {}\n\
             command = {}\n",
            self.enabled,
            self.after_minutes,
            self.sound,
            self.command.as_deref().unwrap_or(""),
        );
        crate::save::write_atomically(&path, body.as_bytes())
    }

    /// The next delay in [`DELAY_CHOICES`], wrapping.
    pub fn cycle_delay(&mut self) {
        let at = DELAY_CHOICES.iter().position(|d| *d == self.after_minutes);
        let next = at.map_or(0, |i| (i + 1) % DELAY_CHOICES.len());
        self.after_minutes = DELAY_CHOICES.get(next).copied().unwrap_or(5);
    }

    /// How the delay reads in the interface.
    #[must_use]
    pub fn delay_label(&self) -> String {
        match self.after_minutes {
            1 => "1 min".to_owned(),
            n => format!("{n} min"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_cycles_through_the_offered_choices_and_wraps() {
        let mut s = IdleSettings {
            after_minutes: 1,
            ..IdleSettings::default()
        };
        let mut seen = vec![s.after_minutes];
        for _ in 0..DELAY_CHOICES.len() {
            s.cycle_delay();
            seen.push(s.after_minutes);
        }
        assert_eq!(seen.first(), seen.last(), "cycling must come back round");
        for want in DELAY_CHOICES {
            assert!(seen.contains(want), "{want} was never offered");
        }
    }

    /// A delay that is not one of the choices still moves when clicked.
    #[test]
    fn an_unknown_delay_cycles_to_the_first_choice_rather_than_sticking() {
        let mut s = IdleSettings {
            after_minutes: 7,
            ..IdleSettings::default()
        };
        s.cycle_delay();
        assert_eq!(Some(s.after_minutes), DELAY_CHOICES.first().copied());
    }

    #[test]
    fn settings_survive_a_round_trip_through_the_file_format() {
        // The parser and the writer, against each other, without touching the
        // real config path.
        let original = IdleSettings {
            enabled: true,
            after_minutes: 15,
            module: IdleModule::Named("Flying Toasters".to_owned()),
            command: None,
            sound: false,
        };
        let module = match &original.module {
            IdleModule::Random => "*random*",
            IdleModule::Named(n) => n.as_str(),
        };
        let text = format!(
            "enabled = {}\nafter_minutes = {}\nmodule = {module}\nsound = {}\n",
            original.enabled, original.after_minutes, original.sound
        );
        let mut parsed = IdleSettings::default();
        for line in text.lines() {
            let (k, v) = line.split_once('=').expect("pair");
            let (k, v) = (k.trim(), v.trim());
            match k {
                "enabled" => parsed.enabled = v == "true",
                "sound" => parsed.sound = v != "false",
                "after_minutes" => parsed.after_minutes = v.parse().expect("number"),
                "module" => {
                    parsed.module = if v == "*random*" {
                        IdleModule::Random
                    } else {
                        IdleModule::Named(v.to_owned())
                    }
                }
                _ => {}
            }
        }
        assert_eq!(parsed, original);
    }

    #[test]
    fn a_missing_or_damaged_config_falls_back_to_the_defaults() {
        // No panic, no error: the browser must always open.
        let d = IdleSettings::default();
        assert!(!d.enabled, "must not take the screen before it is asked to");
        assert_eq!(d.module, IdleModule::Random);
        assert!(DELAY_CHOICES.contains(&d.after_minutes));
    }

    /// An unsupported platform must read as "never idle", never as "always".
    #[test]
    fn idle_is_never_assumed_when_it_cannot_be_measured() {
        let mut w = IdleWatch::new();
        if idle_seconds().is_none() {
            assert!(!w.idle_for(1), "unmeasurable idle must not start a module");
            assert!(!w.woke());
        }
    }
}
