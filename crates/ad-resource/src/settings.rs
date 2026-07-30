//! After Dark 2.0 settings resources.
//!
//! Authoritative source: `GraphicsModule_Types.r` from Berkeley Systems' SDK
//! (`reference/sdk/` and `docs/LEARNINGS.md`).
//!
//! The control panel renders up to four controls per module entirely from these
//! resources — no module code runs to draw them. The values are then handed to
//! the module as `GMParamBlock.controlValues[4]`, indexed by **resource ID minus
//! 1000**. Implementing this vocabulary gives native settings UI for every
//! module at once.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fork::{Resource, ResourceFork};
use crate::macroman;

/// Lowest control resource ID. `controlValues[i]` ⟷ resource ID `1000 + i`.
pub const CONTROL_ID_BASE: i16 = 1000;
/// After Dark shows at most four controls per module.
pub const MAX_CONTROLS: usize = 4;

/// One label/threshold pair from an `sUnt` resource.
///
/// The control panel shows `text` while the slider value is `>= lower_limit` and
/// below the next entry's limit, which is how arbitrarily scaled sliders work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliderUnit {
    pub lower_limit: i16,
    pub text: String,
}

/// A single settings control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// `sVal` — slider, value 0..=100. Label comes from the resource name.
    Slider {
        label: Option<String>,
        value: i16,
        /// Matching `sUnt` entries, if any.
        units: Vec<SliderUnit>,
    },
    /// `bVal` — button. The stored value **is** the `message` selector the host
    /// sends to `main()` when clicked (`ButtonMessage` = 8, so 8..=11).
    Button {
        label: Option<String>,
        message: i16,
    },
    /// `mVal` — pop-up menu; value is the selected item number.
    Menu {
        label: Option<String>,
        value: i16,
    },
    /// `xVal` — check box; 1 checked, 0 unchecked.
    CheckBox {
        label: Option<String>,
        checked: bool,
    },
    /// `tVal` — static text; value is a 1-based index into a matching `STR#`
    /// (0 shows nothing).
    Text {
        label: Option<String>,
        str_index: i16,
    },
}

impl Control {
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Slider { label, .. }
            | Self::Button { label, .. }
            | Self::Menu { label, .. }
            | Self::CheckBox { label, .. }
            | Self::Text { label, .. } => label.as_deref(),
        }
    }

    /// The raw word passed to the module in `controlValues[]`.
    #[must_use]
    pub fn raw_value(&self) -> i16 {
        match self {
            Self::Slider { value, .. } | Self::Menu { value, .. } => *value,
            Self::Button { message, .. } => *message,
            Self::CheckBox { checked, .. } => i16::from(*checked),
            Self::Text { str_index, .. } => *str_index,
        }
    }
}

/// Which selectors a module understands, from `Cals 0`.
///
/// **If the resource is absent, only the original four are supported** — that is
/// the documented default, not an assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub initialize: bool,
    pub close: bool,
    pub blank: bool,
    pub draw_frame: bool,
    pub module_selected: bool,
    pub do_about: bool,
}

impl Default for Capabilities {
    /// The documented default when `Cals` is absent.
    fn default() -> Self {
        Self {
            initialize: true,
            close: true,
            blank: true,
            draw_frame: true,
            module_selected: false,
            do_about: false,
        }
    }
}

impl Capabilities {
    /// Decode a `Cals` byte.
    ///
    /// Bit order follows the Rez template, which declares booleans
    /// most-significant first: two reserved bits, then `DoAbout`,
    /// `ModuleSelected`, `DrawFrame`, `Blank`, `Close`, `Initialize`.
    #[must_use]
    pub fn from_cals(byte: u8) -> Self {
        Self {
            do_about: byte & (1 << 5) != 0,
            module_selected: byte & (1 << 4) != 0,
            draw_frame: byte & (1 << 3) != 0,
            blank: byte & (1 << 2) != 0,
            close: byte & (1 << 1) != 0,
            initialize: byte & 1 != 0,
        }
    }
}

/// Sound configuration from `Chnl 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundConfig {
    /// Which kind of sound channel the module wants reserved.
    pub channel_kind: i16,
    pub volume: i16,
}

/// Memory requirements from `sysz`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryRequest {
    /// `sysz 0` — system heap expansion, and the module's desired memory.
    pub desired: Option<u32>,
    /// `sysz 1` — the module's absolute minimum.
    pub minimum: Option<u32>,
    /// Any other `sysz` id found. Lunatic Fringe ships `sysz 128`, which is
    /// off-spec; combined with its help text asking for ~600 K it is a reminder
    /// **not to size emulated RAM from `sysz` alone**.
    pub off_spec: Vec<(i16, u32)>,
}

/// Everything the control panel and host need that is not code.
#[derive(Debug, Clone, Default)]
pub struct ModuleSettings {
    /// Controls by slot (`0..4`), i.e. resource ID `1000 + slot`. Slots may be
    /// sparse — the SDK permits non-contiguous IDs.
    pub controls: [Option<Control>; MAX_CONTROLS],
    pub capabilities: Capabilities,
    pub sound: Option<SoundConfig>,
    pub memory: MemoryRequest,
}

impl ModuleSettings {
    /// The `controlValues[4]` array to marshal into the emulated param block.
    ///
    /// Empty slots are zero, matching an uninitialised control panel.
    #[must_use]
    pub fn control_values(&self) -> [i16; MAX_CONTROLS] {
        let mut out = [0i16; MAX_CONTROLS];
        for (slot, ctl) in self.controls.iter().enumerate() {
            if let (Some(dst), Some(c)) = (out.get_mut(slot), ctl.as_ref()) {
                *dst = c.raw_value();
            }
        }
        out
    }

    /// Button selectors this module exposes, e.g. Lunatic Fringe's
    /// `Keys…` → 8 and `Clear Scores…` → 9.
    #[must_use]
    pub fn buttons(&self) -> Vec<(i16, Option<&str>)> {
        self.controls
            .iter()
            .flatten()
            .filter_map(|c| match c {
                Control::Button { message, label } => Some((*message, label.as_deref())),
                _ => None,
            })
            .collect()
    }

    /// Decode every settings resource in a fork.
    #[must_use]
    pub fn from_fork(fork: &ResourceFork<'_>) -> Self {
        let mut out = Self::default();

        let slot_of = |id: i16| -> Option<usize> {
            let slot = id.checked_sub(CONTROL_ID_BASE)?;
            (0..MAX_CONTROLS as i16)
                .contains(&slot)
                .then_some(slot as usize)
        };

        // `sUnt` first, so sliders can attach their unit labels.
        let mut units: Vec<(i16, Vec<SliderUnit>)> = Vec::new();
        for r in fork.of_type(b"sUnt") {
            units.push((r.id, parse_sunt(r.data)));
        }

        for r in fork.of_type(b"sVal") {
            if let (Some(slot), Some(value)) = (slot_of(r.id), r.be_i16(0)) {
                let u = units
                    .iter()
                    .find(|(id, _)| *id == r.id)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                if let Some(dst) = out.controls.get_mut(slot) {
                    *dst = Some(Control::Slider {
                        label: r.name.clone(),
                        value,
                        units: u,
                    });
                }
            }
        }
        for r in fork.of_type(b"bVal") {
            if let (Some(slot), Some(message)) = (slot_of(r.id), r.be_i16(0)) {
                if let Some(dst) = out.controls.get_mut(slot) {
                    *dst = Some(Control::Button {
                        label: r.name.clone(),
                        message,
                    });
                }
            }
        }
        for r in fork.of_type(b"mVal") {
            if let (Some(slot), Some(value)) = (slot_of(r.id), r.be_i16(0)) {
                if let Some(dst) = out.controls.get_mut(slot) {
                    *dst = Some(Control::Menu {
                        label: r.name.clone(),
                        value,
                    });
                }
            }
        }
        for r in fork.of_type(b"xVal") {
            if let (Some(slot), Some(v)) = (slot_of(r.id), r.be_i16(0)) {
                if let Some(dst) = out.controls.get_mut(slot) {
                    *dst = Some(Control::CheckBox {
                        label: r.name.clone(),
                        checked: v != 0,
                    });
                }
            }
        }
        for r in fork.of_type(b"tVal") {
            if let (Some(slot), Some(str_index)) = (slot_of(r.id), r.be_i16(0)) {
                if let Some(dst) = out.controls.get_mut(slot) {
                    *dst = Some(Control::Text {
                        label: r.name.clone(),
                        str_index,
                    });
                }
            }
        }

        out.capabilities = fork
            .get(b"Cals", 0)
            .and_then(|r| r.data.first().copied())
            .map_or_else(Capabilities::default, Capabilities::from_cals);

        out.sound = fork.get(b"Chnl", 0).and_then(|r| {
            Some(SoundConfig {
                channel_kind: r.be_i16(0)?,
                volume: r.be_i16(2)?,
            })
        });

        for r in fork.of_type(b"sysz") {
            let Some(v) = r.be_u32(0) else { continue };
            match r.id {
                0 => out.memory.desired = Some(v),
                1 => out.memory.minimum = Some(v),
                other => out.memory.off_spec.push((other, v)),
            }
        }

        out
    }
}

/// Parse an `sUnt` resource: `i16 count`, then `count` × (`i16` limit, Pascal string).
#[must_use]
fn parse_sunt(data: &[u8]) -> Vec<SliderUnit> {
    let mut out = Vec::new();
    let Some(count) = data.get(0..2).and_then(macroman::be_i16) else {
        return out;
    };
    let mut pos = 2usize;
    for _ in 0..count.max(0) {
        let Some(limit) = data
            .get(pos..pos.saturating_add(2))
            .and_then(macroman::be_i16)
        else {
            break;
        };
        pos = pos.saturating_add(2);
        let Some((text, used)) = macroman::pascal_string(data, pos) else {
            break;
        };
        pos = pos.saturating_add(used);
        out.push(SliderUnit {
            lower_limit: limit,
            text,
        });
    }
    out
}

/// Convenience: is this resource type part of the settings vocabulary?
#[must_use]
pub fn is_settings_type(res_type: &[u8; 4]) -> bool {
    matches!(
        res_type,
        b"sVal" | b"sUnt" | b"bVal" | b"mVal" | b"xVal" | b"tVal"
            | b"\xB5Val" | b"sysz" | b"Cals" | b"Chnl" | b"sReq" | b"Actv"
            | b"HVof" | b"fRtS" | b"Ignr"
    )
}

/// All settings resources found in a fork, for the resource inspector.
#[must_use]
pub fn settings_resources<'f, 'a>(
    fork: &'f ResourceFork<'a>,
) -> Vec<&'f Resource<'a>> {
    fork.all()
        .iter()
        .filter(|r| is_settings_type(&r.res_type))
        .collect()
}
