//! Host-side policy: where files live, what gets logged, how saves are made
//! durable.
//!
//! # Why this is a separate crate
//!
//! `ad-toolbox` and `ad-host-v2` emulate a machine. They should not know what a
//! filesystem path is, which is why the Toolbox takes a
//! [`ad_toolbox::resources::ResourceSink`] rather than a directory, and why the
//! diagnostic switches reach it as a typed [`ad_toolbox::Diagnostics`] rather
//! than as `std::env::var` calls buried in a trap handler. Reading the
//! environment from inside a library is fine for a lab and wrong for something
//! that will be loaded into a screen-saver host process; this crate is the one
//! place that does it.
//!
//! # What lives here
//!
//! * [`RuntimeOptions`] — every switch, typed, with `Default` meaning "a
//!   product build": no logging, saves enabled.
//! * [`save_dir`] — the per-platform location for saved state.
//! * [`ForkSink`] — durable, atomic resource writes.
//! * [`Mixer`] — played sounds turned into samples, and, behind the `audio`
//!   feature, [`device::AudioDevice`], which puts them on a speaker.

pub mod audio;
#[cfg(feature = "audio")]
pub mod device;
pub mod dialog;
pub mod idle;
pub mod library;
pub mod pace;
pub mod png;
pub mod save;

pub use audio::Mixer;
#[cfg(feature = "audio")]
pub use device::AudioDevice;
pub use dialog::{
    ask, choose_file_or_folder, choose_folder, copy_to_clipboard, display_size, open_url,
};
pub use idle::{IdleModule, IdleSettings, IdleWatch};
pub use library::{Installed, have_library, install_from, install_into, library_dir};
pub use pace::Pacer;
pub use save::{ForkSink, Imported, export_scores, import_scores, save_dir};

use std::path::PathBuf;

/// Everything the host can be told to do differently, in one typed value.
///
/// `Default` is the product configuration. The lab overrides fields; nothing
/// reads the environment except [`RuntimeOptions::from_env`].
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    /// Diagnostic switches handed to the Toolbox.
    pub diagnostics: ad_toolbox::Diagnostics,
    /// Where saved state goes. `None` disables durable writes entirely, which is
    /// what the compatibility lab wants: 66 modules must not leave files behind,
    /// and a survey that writes to a shared location is not reproducible.
    pub save_dir: Option<PathBuf>,
    /// Cycles per message before the host calls a module hung. `None` keeps the
    /// host's default.
    pub cycle_budget: Option<u32>,
    /// Report achieved frame rate and pacing periodically.
    ///
    /// "It feels laggy" and "it is running at 24 fps" are the same report, and
    /// only one of them can be acted on. This turns the first into the second.
    pub stats: bool,
    /// Emulated processor clock in hertz. `None` keeps the profile's.
    ///
    /// Exposed because it is the one setting with no correct value: After Dark
    /// ran on everything from an 8 MHz Mac Plus to a 40 MHz Quadra, so a module's
    /// animation speed was whatever the machine gave it. See
    /// `ad_toolbox::profile::MachineProfile::clock_hz`.
    pub clock_hz: Option<u32>,
}

impl RuntimeOptions {
    /// The lab configuration, built from `AD_*` environment variables.
    ///
    /// This is the **only** function in the workspace that reads them, and it is
    /// called from binaries, never from a library. The variables are a debugging
    /// interface, not a product one:
    ///
    /// | variable | effect |
    /// |---|---|
    /// | `AD_QD_LOG` | log QuickDraw and Resource Manager activity |
    /// | `AD_WATCH_SCREEN` | log the first write to screen memory from each PC |
    /// | `AD_WATCH_ADDR=<hex>[+<len>]` | log writes into an address range |
    /// | `AD_WATCH_PC=<hex>` | log what the instruction at that PC stores |
    /// | `AD_TRACE_EVENT` | trace the first event-path callback |
    /// | `AD_SAVE_DIR=<path>` | save state here; **absent means no saves at all** |
    /// | `AD_BUDGET=<millions>` | cycles per message before "hung" |
    /// | `AD_MHZ=<4..50>` | emulated processor clock; sets how fast modules run |
    /// | `AD_STATS` | print achieved frame rate and pacing every second |
    #[must_use]
    pub fn from_env() -> Self {
        let flag = |name: &str| std::env::var_os(name).is_some();
        Self {
            diagnostics: ad_toolbox::Diagnostics {
                qd_log: flag("AD_QD_LOG"),
                watch_screen: flag("AD_WATCH_SCREEN"),
                watch_addr: std::env::var("AD_WATCH_ADDR")
                    .ok()
                    .and_then(|s| parse_watch_addr(&s)),
                watch_pc: std::env::var("AD_WATCH_PC")
                    .ok()
                    .and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()),
                trace_event: flag("AD_TRACE_EVENT"),
            },
            // Deliberately *not* the platform save directory. This is the lab's
            // constructor, and a 66-module survey that writes into the user's
            // Application Support folder is both polluting and no longer
            // reproducible — a second run would start from the first run's saved
            // state. Saving from the lab is opt-in via `AD_SAVE_DIR`; the product
            // shells use `product()`.
            save_dir: std::env::var_os("AD_SAVE_DIR").map(PathBuf::from),
            cycle_budget: std::env::var("AD_BUDGET")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .map(|m| m.saturating_mul(1_000_000)),
            // Megahertz, because that is the unit anyone thinks in here. Clamped
            // to a range real 680x0 Macs occupied: below 4 MHz nothing animates,
            // and above 50 the pacer stops being a ceiling on a modern host, so
            // both ends would silently stop meaning what the name says.
            stats: flag("AD_STATS"),
            clock_hz: std::env::var("AD_MHZ")
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|m| (4.0..=50.0).contains(m))
                .map(|m| (m * 1_000_000.0) as u32),
        }
    }

    /// Options with no side effects at all: no logging, no files written.
    ///
    /// What the compatibility survey uses, so a run cannot depend on, or leave
    /// behind, anything outside the process.
    #[must_use]
    pub fn hermetic() -> Self {
        Self::default()
    }

    /// The product configuration: no logging, saves in the platform location.
    ///
    /// What a screen saver or a standalone module launcher wants — the user's
    /// high scores persist across runs and nothing is logged to a console
    /// nobody is reading.
    #[must_use]
    pub fn product() -> Self {
        Self {
            save_dir: save_dir(),
            ..Self::default()
        }
    }

    /// The product configuration, with the `AD_*` switches still honoured.
    ///
    /// What an interactive host wants. [`Self::product`] alone silently ignores
    /// the environment, which turned a documented flag into a lie: `AD_MHZ` was
    /// published in the README as the way to change the emulated clock and the
    /// player never read it, and `AD_STATS` produced no output for the same
    /// reason. Saves still default to the platform location; `AD_SAVE_DIR`
    /// overrides it.
    #[must_use]
    pub fn product_from_env() -> Self {
        let env = Self::from_env();
        Self {
            save_dir: env.save_dir.clone().or_else(save_dir),
            ..env
        }
    }

    /// `AD_QD_LOG` and friends, but never writing files.
    ///
    /// The survey's configuration, spelled out so the guarantee is a function
    /// rather than a convention someone has to remember.
    #[must_use]
    pub fn from_env_hermetic() -> Self {
        Self {
            save_dir: None,
            ..Self::from_env()
        }
    }
}

/// Resource forks that may hold `FONT`/`NFNT` strikes, beside the modules.
///
/// Fonts are not part of a module: a module calling `TextFont(0)` asks for the
/// system font, which it has never seen and does not carry. They came from the
/// System file, so the host supplies them — the same arrangement as `KCHR`.
///
/// Nothing is bundled. These are Apple's fonts out of the user's own disk image,
/// read from wherever the modules were extracted to, and a machine without them
/// simply has no text. Returned as bytes so `ad-toolbox` never sees a path.
#[must_use]
pub fn font_forks(dir: &std::path::Path) -> Vec<Vec<u8>> {
    // The System file first: its `FONT` ids encode family and size, which is what
    // makes an exact `TextFont`/`TextSize` match possible. Suitcases hold `NFNT`s
    // whose identity only a `FOND` knows, so they are fallbacks.
    ["System.rsrc", "Chicago.rsrc", "Geneva.rsrc", "Monaco.rsrc"]
        .iter()
        .filter_map(|name| std::fs::read(dir.join(name)).ok())
        .collect()
}

/// Parse `<hex>[+<len>]` into an inclusive-exclusive address range.
///
/// A range rather than a single address because the recurring diagnostic
/// question is "what wrote over this *structure*", not "this byte".
fn parse_watch_addr(spec: &str) -> Option<(u32, u32)> {
    let (base, len) = match spec.split_once('+') {
        Some((b, l)) => (b, l.parse::<u32>().unwrap_or(1)),
        None => (spec, 1),
    };
    let from = u32::from_str_radix(base.trim().trim_start_matches("0x"), 16).ok()?;
    Some((from, from.saturating_add(len.max(1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_addr_accepts_a_bare_address_and_a_length() {
        assert_eq!(parse_watch_addr("0x1000"), Some((0x1000, 0x1001)));
        assert_eq!(parse_watch_addr("1000+16"), Some((0x1000, 0x1010)));
        // A zero length would watch nothing, which is never what was meant.
        assert_eq!(parse_watch_addr("1000+0"), Some((0x1000, 0x1001)));
        assert_eq!(parse_watch_addr("nonsense"), None);
    }

    #[test]
    fn default_options_are_a_product_build() {
        let o = RuntimeOptions::default();
        assert!(!o.diagnostics.qd_log);
        assert!(o.diagnostics.watch_addr.is_none());
        // The default *value* saves nowhere; `from_env` is what resolves a
        // platform location. A library that constructs options by hand must not
        // accidentally start writing to the user's home directory.
        assert!(o.save_dir.is_none());
    }
}
