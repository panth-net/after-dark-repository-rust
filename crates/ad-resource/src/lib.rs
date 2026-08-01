//! Classic Macintosh resource forks and After Dark module structure.
//!
//! This crate is the bottom of the After Dark runtime: it turns the bytes of an
//! original module into typed structures, and nothing else. It does not execute
//! code, allocate emulated memory, or draw.
//!
//! # Design constraints
//!
//! * **No dependencies.** Every byte here comes from untrusted input, so the
//!   crate stays trivially auditable and fuzzable.
//! * **No panics on malformed input.** Every read is bounds-checked and returns
//!   [`Error`]. A silently truncated resource would render as a subtly wrong
//!   sprite instead of a visible failure, which is worse than an error.
//! * **Byte-exactness is preserved.** Payloads are borrowed, never copied or
//!   normalised; resource names keep their original bytes alongside the decoded
//!   MacRoman.
//!
//! # Example
//!
//! ```no_run
//! use ad_resource::{AdModule, ModuleSettings, ResourceFork};
//!
//! let bytes = std::fs::read("Lunatic Fringe.rsrc")?;
//! let fork = ResourceFork::parse(&bytes)?;
//! let module = AdModule::new(fork);
//!
//! println!("{:?}", module.title());
//! let settings = ModuleSettings::from_fork(module.fork());
//! for (message, label) in settings.buttons() {
//!     println!("button {label:?} sends message {message}");
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![no_std]
#![cfg_attr(test, allow(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

extern crate alloc;

pub mod error;
pub mod font;
pub mod fork;
pub mod hfs;
pub mod macbinary;
pub mod macroman;
pub mod module;
pub mod settings;
pub mod write;

pub use error::{Error, Result};
pub use font::{BitmapFont, Glyph};
pub use fork::{Resource, ResourceFork};
pub use hfs::{ForkFile, resource_forks};
pub use macbinary::{MacBinary, is_macbinary};
pub use module::{
    AdModule, CodeHeader, CodeLayout, GmMessage, GmResult, SegmentHeader, TYPE_ADGM, TYPE_CCOD,
};
pub use settings::{Capabilities, Control, MemoryRequest, ModuleSettings, SliderUnit, SoundConfig};
pub use write::{OwnedResource, write_fork};

/// Finder type of an After Dark graphics module file.
pub const FINDER_TYPE_MODULE: [u8; 4] = *b"ADgm";
/// Finder creator signature of After Dark.
pub const FINDER_CREATOR_AFTERDARK: [u8; 4] = *b"ADrk";
