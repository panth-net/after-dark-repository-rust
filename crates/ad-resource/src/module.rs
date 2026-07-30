//! After Dark graphics module (`ADgm`) structure and calling convention.
//!
//! Authoritative source: Berkeley Systems' own SDK — `GraphicsModule_main.c` and
//! `GraphicsModule_Types.h` (see `reference/sdk/` and `docs/LEARNINGS.md`).
//!
//! The host calls one Pascal-convention entry point:
//!
//! ```c
//! pascal OSErr main(
//!     Handle          *storage,   /* 0x12(A6)  VAR — storage the module allocates */
//!     RgnHandle        blankRgn,  /* 0x0E(A6)  region covering all screens        */
//!     short            message,   /* 0x0C(A6)  the selector                       */
//!     GMParamBlockPtr  params      /* 0x08(A6)  parameters & host services        */
//! );
//! ```
//!
//! 14 bytes of parameters, a 16-bit result, and the callee pops.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fork::{Resource, ResourceFork};
use crate::macroman;

/// Resource type of an After Dark graphics module's code.
pub const TYPE_ADGM: [u8; 4] = *b"ADgm";
/// Resource type of the segmented code resources (classic `CODE` in disguise).
pub const TYPE_CCOD: [u8; 4] = *b"CCOD";

/// Selectors the host passes in `message`.
///
/// Values are from `GMMessage` in `GraphicsModule_Types.h`. Note the order:
/// `Close` is **1** and `Blank` is **2**. Getting this wrong tears down the
/// module's storage on the first frame instead of drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum GmMessage {
    /// Allocate storage and get started. Called with `*storage == nil`.
    Initialize = 0,
    /// Deallocate storage and shut down.
    Close = 1,
    /// Blank the screen.
    Blank = 2,
    /// Draw one frame. Called repeatedly until wake-up.
    DrawFrame = 3,
    /// Module was selected in the control panel, before controls are shown.
    ModuleSelected = 4,
    /// Host is showing the module's help window.
    DoAbout = 5,
    /// A settings button was clicked. `ButtonMessage + n` for button `n`.
    ButtonMessage = 8,
}

impl GmMessage {
    /// The lowest button message. `bVal` resources store one of these directly.
    pub const BUTTON_BASE: i16 = 8;

    #[must_use]
    pub fn from_raw(v: i16) -> Option<Self> {
        Some(match v {
            0 => Self::Initialize,
            1 => Self::Close,
            2 => Self::Blank,
            3 => Self::DrawFrame,
            4 => Self::ModuleSelected,
            5 => Self::DoAbout,
            v if v >= Self::BUTTON_BASE => Self::ButtonMessage,
            _ => return None,
        })
    }
}

/// Values a module may return from `main`.
///
/// From the anonymous `enum` in `GraphicsModule_Types.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmResult {
    /// `noErr` — carry on.
    Ok,
    /// `ModuleError` (-1) — host displays `params->errorMessage`.
    ModuleError,
    /// `RestartMe` (1) — host re-sends `Initialize`.
    RestartMe,
    /// `ImDone` (2) — host stops calling and takes over drawing.
    ImDone,
    /// `RefreshResources` (3) — host redraws controls; valid only after a SetUp message.
    RefreshResources,
    /// Anything else the module returned.
    Other(i16),
}

impl GmResult {
    #[must_use]
    pub fn from_raw(v: i16) -> Self {
        match v {
            0 => Self::Ok,
            -1 => Self::ModuleError,
            1 => Self::RestartMe,
            2 => Self::ImDone,
            3 => Self::RefreshResources,
            other => Self::Other(other),
        }
    }
}

/// `systemConfig` bits in `GMParamBlock`.
pub mod system_config {
    /// Sound is available.
    pub const SOUND_AVAILABLE: u16 = 1 << 15;
    /// After Dark extensions are present.
    pub const EXTENSIONS_AVAILABLE: u16 = 1 << 14;
    /// MultiModule is running.
    pub const MULTI_MODULE_RUNNING: u16 = 1 << 10;
    /// The module must not animate.
    pub const MODULE_MAY_NOT_ANIMATE: u16 = 1 << 9;
}

/// Byte offsets within `GMParamBlock`, for marshalling into emulated memory.
///
/// Classic 68K Mac alignment: `Boolean` occupies one byte and the following
/// `short` is padded to an even offset.
pub mod param_block {
    /// `short controlValues[4]` — the four user slider/checkbox/menu values.
    pub const CONTROL_VALUES: usize = 0;
    /// `MonitorsInfoPtr monitors`
    pub const MONITORS: usize = 8;
    /// `Boolean colorQDAvail` (padded to a word)
    pub const COLOR_QD_AVAIL: usize = 12;
    /// `short systemConfig`
    pub const SYSTEM_CONFIG: usize = 14;
    /// `QDGlobalsPtr qdGlobalsCopy`
    pub const QD_GLOBALS_COPY: usize = 16;
    /// `short brightness`
    pub const BRIGHTNESS: usize = 20;
    /// `Rect demoRect`
    pub const DEMO_RECT: usize = 22;
    /// `StringPtr errorMessage`
    pub const ERROR_MESSAGE: usize = 30;
    /// `SndChannelPtr sndChannel`
    pub const SND_CHANNEL: usize = 34;
    /// `short adVersion` (BCD)
    pub const AD_VERSION: usize = 38;
    /// `ExtensionTablePtr extensions`
    pub const EXTENSIONS: usize = 40;
    /// Total size in bytes.
    pub const SIZE: usize = 44;
}

/// The 16-byte header most `ADgm` resources begin with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeHeader {
    /// The resource ID the header claims (compare against the actual ID).
    pub declared_id: i16,
}

/// How a module's code resource is laid out and where execution starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLayout {
    /// Present on 56 of the 66 modules on the After Dark 2.0x disk.
    pub header: Option<CodeHeader>,
    /// Byte offset of the Pascal entry point within the resource.
    pub entry_offset: usize,
    /// True when the `LEA/NOP/NOP/BRA.W` stub was decoded to find `entry_offset`.
    pub resolved_via_stub: bool,
}

/// `BRA.S +14` — jumps over the 16-byte header.
const HEADER_BRA: [u8; 2] = [0x60, 0x0E];
/// `LEA -18(PC),A0`
const STUB_LEA: [u8; 4] = [0x41, 0xFA, 0xFF, 0xEE];
/// `NOP ; NOP`
const STUB_NOPS: [u8; 4] = [0x4E, 0x71, 0x4E, 0x71];
/// `BRA.W` opcode.
const STUB_BRA_W: [u8; 2] = [0x60, 0x00];

impl CodeLayout {
    /// Determine the layout of an `ADgm` code resource.
    ///
    /// Two shapes occur in the wild:
    ///
    /// * **Headered** (56/66): `BRA.S +14`, `'ADgm'`, `i16 id`, padding, then at
    ///   `+16` a stub `LEA -18(PC),A0 ; NOP ; NOP ; BRA.W main`.
    /// * **Bare** (10/66, e.g. Hard Rain, GeoBounce): no header; the Pascal
    ///   prologue starts at offset 0.
    ///
    /// When the header is present but the stub does not match, `entry_offset`
    /// falls back to 16 and `resolved_via_stub` is false — those need a further
    /// stub variant decoded before they can run.
    #[must_use]
    pub fn detect(code: &[u8]) -> Self {
        let has_header = code.get(0..2) == Some(&HEADER_BRA[..])
            && code.get(4..8) == Some(&TYPE_ADGM[..]);
        if !has_header {
            return Self {
                header: None,
                entry_offset: 0,
                resolved_via_stub: false,
            };
        }
        let declared_id = code.get(8..10).and_then(macroman::be_i16).unwrap_or(0);
        let header = Some(CodeHeader { declared_id });

        // Try the standard stub at +16.
        if code.get(16..20) == Some(&STUB_LEA[..])
            && code.get(20..24) == Some(&STUB_NOPS[..])
            && code.get(24..26) == Some(&STUB_BRA_W[..])
        {
            if let Some(disp) = code.get(26..28).and_then(macroman::be_i16) {
                // BRA.W displacement is relative to the address of the extension word.
                let target = 26i64.saturating_add(i64::from(disp));
                if target >= 0 && (target as usize) < code.len() {
                    return Self {
                        header,
                        entry_offset: target as usize,
                        resolved_via_stub: true,
                    };
                }
            }
        }
        Self {
            header,
            entry_offset: 16,
            resolved_via_stub: false,
        }
    }
}

/// An After Dark graphics module, viewed through its resource fork.
#[derive(Debug)]
pub struct AdModule<'a> {
    fork: ResourceFork<'a>,
}

impl<'a> AdModule<'a> {
    #[must_use]
    pub fn new(fork: ResourceFork<'a>) -> Self {
        Self { fork }
    }

    #[must_use]
    pub fn fork(&self) -> &ResourceFork<'a> {
        &self.fork
    }

    /// The module's code resource.
    ///
    /// Located **by type**, never by a fixed ID: observed IDs on the After Dark
    /// 2.0x disk include 0, 12, 63, 128 and 129.
    #[must_use]
    pub fn code(&self) -> Option<&Resource<'a>> {
        self.fork.of_type(&TYPE_ADGM).first().copied()
    }

    /// Segmented code resources, ascending by ID.
    ///
    /// These are classic `CODE` resources renamed so the System does not treat
    /// them as application code: the 4-byte header is
    /// `(u16 jumpTableOffset, u16 entryCount)` and segments chain such that
    /// `offset(n+1) == offset(n) + 8 * count(n)`.
    #[must_use]
    pub fn segments(&self) -> Vec<&Resource<'a>> {
        self.fork.of_type(&TYPE_CCOD)
    }

    /// The module's descriptor from `ADrk 0` — a Pascal string holding the
    /// module name, its own After Dark version and the copyright line, e.g.
    /// `"Hard Rain 2.0\r©1989, 90 Berkeley Systems Inc."`.
    #[must_use]
    pub fn descriptor(&self) -> Option<String> {
        pascal_resource(self.fork.get(b"ADrk", 0)?)
    }

    /// The module's title as shown in the library — the first line of
    /// [`Self::descriptor`], which is `name` + space + version.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        let d = self.descriptor()?;
        Some(d.split('\r').next().unwrap_or(&d).trim().into())
    }

    /// The layout of the module's code resource, if it has one.
    #[must_use]
    pub fn code_layout(&self) -> Option<CodeLayout> {
        Some(CodeLayout::detect(self.code()?.data))
    }

    /// Verify that segment jump-table offsets chain correctly.
    ///
    /// Returns `Err` with `(segment_id, declared, expected)` on the first break.
    /// A broken chain means the A5 jump table cannot be built.
    pub fn verify_segment_chain(&self) -> core::result::Result<(), (i16, u32, u32)> {
        let mut expected: Option<u32> = None;
        for seg in self.segments() {
            let Some(h) = SegmentHeader::parse(seg.data) else {
                continue;
            };
            if let Some(want) = expected {
                if u32::from(h.jump_table_offset) != want {
                    return Err((seg.id, u32::from(h.jump_table_offset), want));
                }
            }
            expected = Some(h.next_offset());
        }
        Ok(())
    }
}

/// A `CCOD` segment header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    /// Offset of this segment's first entry within the A5 jump table.
    pub jump_table_offset: u16,
    /// Number of 8-byte jump table entries this segment owns.
    pub entry_count: u16,
}

impl SegmentHeader {
    /// Bytes of jump table this segment occupies (`8 * entry_count`).
    #[must_use]
    pub fn table_bytes(&self) -> u32 {
        u32::from(self.entry_count).saturating_mul(8)
    }

    /// The jump table offset the *next* segment must declare.
    #[must_use]
    pub fn next_offset(&self) -> u32 {
        u32::from(self.jump_table_offset).saturating_add(self.table_bytes())
    }

    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        Some(Self {
            jump_table_offset: u16::from_be_bytes(
                <[u8; 2]>::try_from(data.get(0..2)?).ok()?,
            ),
            entry_count: u16::from_be_bytes(<[u8; 2]>::try_from(data.get(2..4)?).ok()?),
        })
    }
}

/// Decode a Pascal-string resource such as `ADrk 0`, `STR ` or `MPST`.
#[must_use]
pub fn pascal_resource(res: &Resource<'_>) -> Option<String> {
    macroman::pascal_string(res.data, 0).map(|(s, _)| s)
}
