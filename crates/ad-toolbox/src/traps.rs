//! Trap numbers and names.
//!
//! An A-line opcode is `1010` in the top nibble, then a flag/number field. The
//! two families differ in how the low bits are read:
//!
//! * **OS traps** (`$A000..$A7FF`): the *low byte* is the trap number and bits
//!   8..10 are flags. So `$A11E` and `$A01E` are both `NewPtr`, differing only
//!   in the "don't clear / clear / sys heap" flags. Getting this wrong is how a
//!   dispatcher ends up with hundreds of phantom traps.
//! * **Toolbox traps** (`$A800..$ABFF`): the low **ten** bits are the number and
//!   bit 10 is the auto-pop flag. Masking only nine bits silently folds `$AA31`
//!   (`SetGDevice`) onto `$A831`, so half the Color QuickDraw range disappears.
//!
//! Memory Manager flag bits, from *Inside Macintosh: Memory*:
//! bit 9 (`$0200`) = clear the block, bit 10 (`$0400`) = allocate in the system
//! heap.

/// Which dispatch family a trap word belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Os,
    Toolbox,
}

/// A decoded A-line trap word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trap {
    /// The full opcode word, as it appeared in the instruction stream.
    pub word: u16,
    pub family: Family,
    /// Canonical trap number: low byte for OS traps, low 10 bits for Toolbox.
    pub number: u16,
    /// True when bit 9 is set. For Memory Manager traps this means "clear".
    pub flag_clear: bool,
    /// True when bit 10 is set. For Memory Manager traps this means "sys heap".
    pub flag_sys: bool,
    /// True when bit 11 is set — the auto-pop bit on Toolbox traps.
    pub auto_pop: bool,
}

impl Trap {
    #[must_use]
    pub fn decode(word: u16) -> Self {
        let toolbox = word & 0x0800 != 0;
        if toolbox {
            Self {
                word,
                family: Family::Toolbox,
                number: word & 0x03FF,
                flag_clear: false,
                flag_sys: false,
                auto_pop: word & 0x0400 != 0,
            }
        } else {
            Self {
                word,
                family: Family::Os,
                number: word & 0x00FF,
                flag_clear: word & 0x0200 != 0,
                flag_sys: word & 0x0400 != 0,
                auto_pop: false,
            }
        }
    }

    /// True when this OS trap's flag bits are *modifiers* rather than part of its
    /// identity.
    ///
    /// Easy to get wrong and expensive when you do. For the Memory Manager
    /// allocators the flags select a variant of the same call — `$A122` and
    /// `$A322` are both `NewHandle`, one clearing the block. Elsewhere the same
    /// bits select an entirely *different* call: `$A01D` is `ReserveMem` while
    /// `$A11D` is `MaxMem`. Canonicalising those to a common word silently merges
    /// two unrelated traps, so dispatch keys on the raw word except for the
    /// allocators listed here.
    #[must_use]
    pub fn flags_are_modifiers(&self) -> bool {
        matches!(self.family, Family::Os) && matches!(self.number, 0x1E | 0x22)
    }

    /// The canonical word for this trap with all flags cleared, which is what
    /// the name table is keyed on.
    #[must_use]
    pub fn canonical(&self) -> u16 {
        match self.family {
            Family::Os => 0xA000 | self.number,
            Family::Toolbox => 0xA800 | self.number,
        }
    }

    /// Human-readable name, or `None` if unknown.
    #[must_use]
    pub fn name(&self) -> Option<&'static str> {
        name_of(self.canonical())
    }

    /// A diagnostic label that always says something useful.
    #[must_use]
    pub fn label(&self) -> String {
        match self.name() {
            Some(n) => format!("_{n} (${:04X})", self.word),
            None => format!("unknown trap ${:04X}", self.word),
        }
    }
}

/// Names for the traps this runtime knows about.
///
/// Deliberately not exhaustive: it covers what After Dark 2.0x modules were
/// measured to call (see `docs/LEARNINGS.md`) plus the surrounding Memory, Resource and
/// QuickDraw calls. An unknown trap still reports its number, so the table being
/// incomplete never hides a failure.
#[must_use]
pub fn name_of(canonical: u16) -> Option<&'static str> {
    Some(match canonical {
        // ---- Memory Manager (OS) ----
        0xA01E => "NewPtr",
        0xA01F => "DisposPtr",
        0xA020 => "SetPtrSize",
        0xA021 => "GetPtrSize",
        0xA022 => "NewHandle",
        0xA023 => "DisposHandle",
        0xA024 => "SetHandleSize",
        0xA025 => "GetHandleSize",
        0xA026 => "HandleZone",
        0xA027 => "ReallocHandle",
        0xA028 => "RecoverHandle",
        0xA029 => "HLock",
        0xA02A => "HUnlock",
        0xA02B => "EmptyHandle",
        0xA02C => "InitApplZone",
        0xA02D => "SetApplLimit",
        0xA02E => "BlockMove",
        0xA049 => "HPurge",
        0xA04A => "HNoPurge",
        0xA04C => "CompactMem",
        0xA04D => "PurgeMem",
        0xA05C => "MemoryDispatch",
        0xA061 => "MaxBlock",
        0xA063 => "MaxApplZone",
        0xA064 => "MoveHHi",
        0xA065 => "StackSpace",
        0xA066 => "NewEmptyHandle",
        0xA069 => "HGetState",
        0xA06A => "HSetState",
        0xA01C => "FreeMem",
        0xA01D => "ReserveMem",
        0xA11D => "MaxMem",
        0xA01A => "SetApplBase",
        0xA11A => "GetZone",
        0xA161 => "MaxBlockSys",
        0xA162 => "PurgeSpace",
        0xA030 => "OSEventAvail",
        0xA031 => "GetOSEvent",
        0xA032 => "FlushEvents",
        0xA033 => "VInstall",
        0xA034 => "VRemove",
        0xA06F => "SlotVInstall",
        0xA070 => "SlotVRemove",
        0xA03B => "Delay",
        0xA055 => "StripAddress",
        0xA058 => "InsTime",
        0xA059 => "RmvTime",
        0xA05A => "PrimeTime",
        0xA090 => "SysEnvirons",
        0xA1AD => "Gestalt",
        0xA146 => "GetTrapAddress",
        0xA047 => "SetTrapAddress",
        0xA746 => "GetToolTrapAddress",
        0xA647 => "SetToolTrapAddress",
        0xA346 => "GetOSTrapAddress",
        0xA247 => "SetOSTrapAddress",
        0xA9C6 => "Secs2Date",
        0xA9C7 => "Date2Secs",
        0xA093 => "MemError",
        0xA004 => "Control",

        // ---- File Manager (OS) ----
        //
        // Bit 9 selects the HFS variant of each of these: `$A200` is `_HOpen`,
        // `$A20A` `_HOpenRF`, `$A260` `_HFSDispatch`. The table is keyed on the
        // canonical word so it can only carry the base name; [`Trap::label`]
        // prints the raw word beside it, which is what distinguishes them.
        0xA000 => "Open",
        0xA00A => "OpenRF",
        0xA060 => "FSDispatch",

        // ---- Resource Manager (Toolbox) ----
        0xA81F => "Get1Resource",
        0xA820 => "Get1NamedResource",
        0xA9A0 => "GetResource",
        0xA9A1 => "GetNamedResource",
        0xA9A2 => "LoadResource",
        0xA9A3 => "ReleaseResource",
        0xA9A4 => "HomeResFile",
        0xA9A5 => "SizeRsrc",
        0xA9A6 => "GetResAttrs",
        0xA9A7 => "SetResAttrs",
        0xA9A8 => "GetResInfo",
        0xA9A9 => "SetResInfo",
        0xA9AA => "ChangedResource",
        0xA9AB => "AddResource",
        0xA9AD => "RmveResource",
        0xA9AF => "ResError",
        0xA9B0 => "WriteResource",
        0xA994 => "CurResFile",
        0xA997 => "OpenResFile",
        0xA998 => "UseResFile",
        0xA999 => "UpdateResFile",
        0xA99A => "CloseResFile",
        0xA99B => "SetResLoad",
        0xA99C => "CountResources",
        0xA99D => "GetIndResource",
        0xA99E => "CountTypes",
        0xA99F => "GetIndType",
        0xA9C1 => "UniqueID",
        0xA810 => "Unique1ID",
        0xA80D => "Count1Resources",
        0xA81C => "Count1Types",

        // ---- QuickDraw (Toolbox) ----
        0xA86E => "InitGraf",
        0xA86F => "OpenPort",
        0xA870 => "LocalToGlobal",
        0xA871 => "GlobalToLocal",
        0xA872 => "GrafDevice",
        0xA873 => "SetPort",
        0xA874 => "GetPort",
        0xA875 => "SetPortBits",
        0xA876 => "PortSize",
        0xA877 => "MovePortTo",
        0xA878 => "SetOrigin",
        0xA879 => "SetClip",
        0xA87A => "GetClip",
        0xA87B => "ClipRect",
        0xA87C => "BackPat",
        0xA87D => "ClosePort",
        0xA87E => "AddPt",
        0xA87F => "SubPt",
        0xA880 => "SetPt",
        0xA881 => "EqualPt",
        0xA883 => "DrawChar",
        0xA884 => "DrawString",
        0xA885 => "DrawText",
        0xA886 => "TextWidth",
        0xA887 => "TextFont",
        0xA888 => "TextFace",
        0xA889 => "TextMode",
        0xA88A => "TextSize",
        0xA88B => "GetFontInfo",
        0xA88C => "StringWidth",
        0xA890 => "StdLine",
        0xA891 => "LineTo",
        0xA892 => "Line",
        0xA893 => "MoveTo",
        0xA894 => "Move",
        0xA896 => "HidePen",
        0xA897 => "ShowPen",
        0xA898 => "GetPenState",
        0xA899 => "SetPenState",
        0xA89A => "GetPen",
        0xA89B => "PenSize",
        0xA89C => "PenMode",
        0xA89D => "PenPat",
        0xA89E => "PenNormal",
        0xA8A1 => "FrameRect",
        0xA8A2 => "PaintRect",
        0xA8A3 => "EraseRect",
        0xA8A4 => "InverRect",
        0xA8A5 => "FillRect",
        0xA8A6 => "EqualRect",
        0xA8A7 => "SetRect",
        0xA8A8 => "OffsetRect",
        0xA8A9 => "InsetRect",
        0xA8AA => "SectRect",
        0xA8AB => "UnionRect",
        0xA8AC => "Pt2Rect",
        0xA8AD => "PtInRect",
        0xA8AE => "EmptyRect",
        0xA8B0 => "FrameRoundRect",
        0xA8B1 => "PaintRoundRect",
        0xA8B2 => "EraseRoundRect",
        0xA8B7 => "FrameOval",
        0xA8B8 => "PaintOval",
        0xA8B9 => "EraseOval",
        0xA8BA => "InvertOval",
        0xA8BB => "FillOval",
        0xA8BC => "SlopeFromAngle",
        0xA8BE => "FrameArc",
        0xA8BF => "PaintArc",
        0xA8C6 => "FramePoly",
        0xA8C7 => "PaintPoly",
        0xA8C8 => "ErasePoly",
        0xA8CB => "OpenPoly",
        0xA8CC => "ClosePoly",
        0xA8CD => "KillPoly",
        0xA8CE => "OffsetPoly",
        0xA817 => "CopyMask",
        0xA8CF => "PackBits",
        0xA8D0 => "UnpackBits",
        0xA8D2 => "FrameRgn",
        0xA8D3 => "PaintRgn",
        0xA8D4 => "EraseRgn",
        0xA8D5 => "InverRgn",
        0xA8D6 => "FillRgn",
        0xA8D8 => "NewRgn",
        0xA8D9 => "DisposeRgn",
        0xA8DA => "OpenRgn",
        0xA8DB => "CloseRgn",
        0xA8DC => "CopyRgn",
        0xA8DD => "SetEmptyRgn",
        0xA8DE => "SetRectRgn",
        0xA8DF => "RectRgn",
        0xA8E0 => "OffsetRgn",
        0xA8E1 => "InsetRgn",
        0xA8E2 => "EmptyRgn",
        0xA8E3 => "EqualRgn",
        0xA8E4 => "SectRgn",
        0xA8E5 => "UnionRgn",
        0xA8E6 => "DiffRgn",
        0xA8E7 => "XorRgn",
        0xA8E8 => "PtInRgn",
        0xA8E9 => "RectInRgn",
        0xA8EC => "CopyBits",
        0xA8EF => "ScrollRect",
        0xA8F6 => "DrawPicture",
        0xA8F8 => "ScalePt",
        0xA8F9 => "MapPt",
        0xA8FA => "MapRect",
        0xA8FB => "MapRgn",
        0xA8FC => "MapPoly",
        0xA865 => "GetPixel",
        0xA862 => "ForeColor",
        0xA863 => "BackColor",
        0xA864 => "ColorBit",
        0xA85D => "BitTst",
        0xA858 => "BitAnd",
        0xA85B => "BitOr",
        0xA85A => "BitNot",
        0xA859 => "BitXor",
        0xA85C => "BitShift",
        0xA861 => "Random",
        0xA867 => "LongMul",
        0xA868 => "FixMul",
        0xA869 => "FixRatio",
        0xA86A => "HiWord",
        0xA86B => "LoWord",
        0xA86C => "FixRound",
        0xA84D => "FixDiv",

        // ---- Color QuickDraw ----
        // ---- Menu Manager (Toolbox) ----
        //
        // Anchored by four independently measured call shapes rather than any
        // published table: `$A946` fills a Str255 (MultiModule `_BlockMove`s
        // `buffer[0]+1` bytes out of it), `$A947` takes one, `$A950` maps a
        // MenuHandle to a word, and `$A94B` takes a 32x32 rect and an `_GetIcon`
        // handle. Those pin the whole `$A93F..$A952` run.
        0xA941 => "GetItmStyle",
        0xA946 => "GetItem",
        0xA947 => "SetItem",
        0xA94B => "PlotIcon",
        0xA950 => "CountMItems",
        0xA9BF => "GetMenu",

        // ---- Traps this runtime *handles* but had no name for ----
        //
        // Every one of these was already dispatched, and every diagnostic about
        // one read `_?($AXXX)`. That is not a cosmetic gap: it is what made
        // `$AB66` and `$AC2E` take an afternoon each instead of five minutes, and
        // what made a trap 34 modules use show as `?` in the audit tools. The
        // name of each is taken from the handler that already implements it —
        // these are not new identifications, they are ones that were never
        // written down.
        0xA801 => "SoundDead",
        0xA80E => "SetResPurge",
        0xA82E => "ColorUtilities",
        // Toolbox fixed-point pack: Fixed is 16.16, Fract is 2.30.
        0xA83F => "Long2Fix",
        0xA840 => "Fix2Long",
        0xA841 => "Fix2Frac",
        0xA842 => "Frac2Fix",
        0xA844 => "X2Fix",
        0xA847 => "FracCos",
        0xA848 => "FracSin",
        0xA849 => "FracSqrt",
        0xA84A => "FracMul",
        0xA84B => "FracDiv",
        0xA855 => "ShieldCursor",
        0xA86D => "InitPort",
        0xA88D => "CharWidth",
        0xA88E => "SpaceExtra",
        0xA8B3 => "FrameRoundRect",
        0xA8B4 => "PaintRoundRect",
        0xA8C0 => "PaintArc",
        0xA8C1 => "EraseArc",
        0xA8C2 => "InvertArc",
        0xA8C9 => "ErasePoly",
        0xA8CA => "InvertPoly",
        0xA900 => "GetFNum",
        0xA992 => "DetachResource",
        // Sugar over GetResource with a fixed type.
        0xA9B8 => "GetIndPattern",
        0xA9B9 => "GetCursor",
        0xA9BA => "GetString",
        0xA9BB => "GetIcon",
        0xA9BC => "GetPicture",
        // `_SystemTask` is `$A9B4`. This is **`_KeyTranslate`**, and the wrong name
        // came with a wrong implementation: it was grouped with the cursor no-ops,
        // so it popped none of its ten bytes of arguments and returned nothing.
        //
        // Derived from Lunatic Fringe's call site rather than from a table — the
        // third time a table has been wrong here. Its own MacsBug symbol for the
        // enclosing routine is `CONVERTM`:
        //
        //     pea 'KCHR' ; _GetResource      the keyboard layout
        //     clr.l -(a7)                    LONGINT result slot
        //     move.l (a0), -(a7)             transData: the KCHR pointer
        //     move.w $8(a6), -(a7)           keycode
        //     pea -$8(a6)                    VAR state
        //     $A9C3
        //     move.l (a7)+, d0 ; move.b d0   the low byte is the character
        //
        // which is `KeyTranslate(transData, keycode, VAR state): LONGINT` exactly.
        // Unimplemented, the game showed "N" — none — against every one of its
        // configurable controls.
        0xA9B4 => "SystemTask",
        0xA9C3 => "KeyTranslate",
        // SANE, software floating point. Strange Attractors alone made 179,516
        // `FP68K` calls in one session, and the trap had no name.
        0xA9EB => "FP68K",
        0xA9EC => "Elems68K",
        0xA9EE => "Pack7",
        0xAA01 => "OpenCPort",
        0xAA06 => "SetPortPix",
        0xAA1E => "GetCIcon",
        0xAA21 => "OpColor",
        0xAA4E => "SetStdCProcs",
        // Flag variants that canonicalise onto a named OS trap keep their own
        // entries out of this table; `Trap::canonical` folds them.
        0xA040 => "ReserveMem",
        0xA046 => "GetTrapAddress",
        0xA05D => "SwapMMUMode",
        0xA01B => "SetZone",
        // `_Unimplemented` — referenced by 41 of the 66 modules, which is not a
        // surprise: the way to ask "does this system have that call?" is to
        // compare `GetTrapAddress(x)` against `GetTrapAddress(_Unimplemented)`.
        // The runtime already keys its shared unimplemented slot on this exact
        // word; it just never had a name for diagnostics.
        0xA89F => "Unimplemented",

        // ---- TextEdit (Toolbox) ----
        0xA9D2 => "TENew",
        0xA9CF => "TESetText",
        0xA9D3 => "TEUpdate",
        0xA9CD => "TEDispose",
        0xA9CE => "TextBox",

        0xAA00 => "OpenCPort",
        0xAA03 => "NewPixMap",
        0xAA04 => "DisposPixMap",
        0xAA07 => "NewPixPat",
        0xAA08 => "DisposPixPat",
        0xAA0A => "PenPixPat",
        0xAA0B => "BackPixPat",
        0xAA0D => "MakeRGBPat",
        0xAA1F => "PlotCIcon",
        0xAA25 => "DisposCIcon",
        0xAA3A => "AddSearch",
        0xAA4C => "DelSearch",
        0xAA0E => "FillCRect",
        0xAA12 => "FillCRgn",
        0xAA14 => "RGBForeColor",
        0xAA15 => "RGBBackColor",
        0xAA16 => "SetCPixel",
        0xAA17 => "GetCPixel",
        0xAA18 => "GetCTable",
        0xAA19 => "GetForeColor",
        0xAA1A => "GetBackColor",
        0xAA24 => "DisposCTable",
        0xAA27 => "GetMaxDevice",
        0xAA29 => "GetDeviceList",
        0xAA2A => "GetMainDevice",
        0xAA2B => "GetNextDevice",
        0xAA2C => "TestDeviceAttribute",
        0xAA31 => "SetGDevice",
        0xAA32 => "GetGDevice",
        0xAA33 => "Color2Index",
        0xAA34 => "Index2Color",
        0xAA35 => "InvertColor",
        0xAA36 => "RealColor",
        0xAA3F => "SetEntries",
        0xAA40 => "QDError",
        0xAA53 => "PaletteDispatch",

        // ---- Sound Manager ----
        0xA800 => "SoundDead",
        0xA803 => "SndDisposeChannel",
        0xA804 => "SndAddModifier",
        0xA805 => "SndDoCommand",
        0xA806 => "SndDoImmediate",
        0xA807 => "SndPlay",
        0xA808 => "SndControl",
        0xA809 => "SndNewChannel",
        0xA9C8 => "SysBeep",

        // ---- Event Manager ----
        //
        // This block was shifted by two for a long time — `GetMouse` at $A970,
        // `TickCount` at $A973, `GetKeys` at $A974 — and every entry below was
        // re-derived from the **call sites in the modules themselves**, because
        // the shape of a call identifies the routine and a table cannot lie
        // about it:
        //
        //   $A970  Monitors: `clr.w -(a7); move.w #$ff6f,-(a7); pea.l evt;`
        //          then `move.b (a7)+,d0` — (mask, VAR EventRecord): BOOLEAN.
        //   $A971  Monitors: identical shape.
        //   $A972  After Dark cdev: `pea.l -$72(a6)` then reads a **long**
        //          (a Point) back out of that local — VAR Point, no result.
        //   $A973  After Dark cdev 0x1696: `while (…) { … GetMouse }` — the
        //          drag-tracking idiom, and a no-argument BOOLEAN.
        //   $A974  Life II 0x996: `do {} while (x)` then `do {} while (!x)` —
        //          release-then-press, i.e. `Button`, no arguments.
        //   $A975  Bogglins, ten sites: `clr.l -(a7); trap; move.l (a7)+,field`
        //          — a **four-byte** result with no arguments. Within the Event
        //          Manager only `TickCount` has that shape.
        //   $A976  Strange Attractors: passes `-$10(a6)` (16 bytes) and then
        //          tests bit 1 of the long at +4 — a `KeyMap`.
        //   $A977  Date & Time: no-argument BOOLEAN.
        //
        // The old $A973 = TickCount was the most expensive single error in this
        // runtime: 48 modules call $A975 (260 sites), all of them got a BOOLEAN
        // `false`, so `TickCount` read as 0 forever and every module that paced
        // itself on the tick count froze after its first frame.
        0xA970 => "GetNextEvent",
        0xA971 => "EventAvail",
        0xA972 => "GetMouse",
        0xA973 => "StillDown",
        0xA974 => "Button",
        0xA975 => "TickCount",
        0xA976 => "GetKeys",
        0xA977 => "WaitMouseUp",
        // ---- Windows / Dialogs / Segment Loader ----
        0xA860 => "WaitNextEvent",
        0xA850 => "InitCursor",
        0xA851 => "SetCursor",
        0xA852 => "HideCursor",
        0xA853 => "ShowCursor",
        0xA856 => "ObscureCursor",
        0xA912 => "InitWindows",
        0xA913 => "NewWindow",
        0xA914 => "DisposeWindow",
        0xA98D => "GetDItem",
        0xA98E => "SetDItem",
        0xA98F => "SetIText",
        0xA990 => "GetIText",
        0xA991 => "ModalDialog",
        0xA97B => "InitDialogs",
        0xA9F0 => "LoadSeg",
        0xA9F1 => "UnLoadSeg",
        0xA9FF => "Debugger",
        0xA9C9 => "SysError",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_traps_decode_low_byte_and_flags() {
        // $A11E and $A01E are both NewPtr; $A31E adds "clear".
        for w in [0xA01Eu16, 0xA11E, 0xA31E, 0xA51E] {
            let t = Trap::decode(w);
            assert_eq!(t.family, Family::Os);
            assert_eq!(t.number, 0x1E);
            assert_eq!(t.canonical(), 0xA01E);
            assert_eq!(t.name(), Some("NewPtr"), "for {w:#06x}");
        }
        assert!(!Trap::decode(0xA11E).flag_clear);
        assert!(Trap::decode(0xA31E).flag_clear, "bit 9 means clear");
        assert!(Trap::decode(0xA51E).flag_sys, "bit 10 means system heap");
    }

    #[test]
    fn newhandle_variants_all_resolve() {
        for w in [0xA022u16, 0xA122, 0xA322, 0xA522, 0xA722] {
            let t = Trap::decode(w);
            assert_eq!(t.canonical(), 0xA022);
            assert_eq!(t.name(), Some("NewHandle"), "for {w:#06x}");
        }
        assert!(Trap::decode(0xA322).flag_clear, "NewHandleClear");
    }

    #[test]
    fn toolbox_traps_decode_low_nine_bits() {
        let t = Trap::decode(0xA861);
        assert_eq!(t.family, Family::Toolbox);
        assert_eq!(t.number, 0x061);
        assert_eq!(t.canonical(), 0xA861);
        assert_eq!(t.name(), Some("Random"));

        // Toolbox range extends to $ABFF, so bit 9 is part of the number.
        let t = Trap::decode(0xAA31);
        assert_eq!(t.family, Family::Toolbox);
        assert_eq!(t.canonical(), 0xAA31);
        assert_eq!(t.name(), Some("SetGDevice"));
    }

    #[test]
    fn auto_pop_bit_is_recognised() {
        // Bit 11 set on a Toolbox trap is the auto-pop form.
        let t = Trap::decode(0xAC61);
        assert!(t.auto_pop);
        assert_eq!(t.canonical(), 0xA861, "auto-pop must not change the trap");
    }

    #[test]
    fn unknown_traps_still_label_themselves() {
        let t = Trap::decode(0xA8FF);
        assert_eq!(t.name(), None);
        assert!(t.label().contains("A8FF"), "{}", t.label());
    }

    #[test]
    fn known_traps_label_with_name() {
        assert!(Trap::decode(0xA861).label().contains("Random"));
        assert!(Trap::decode(0xA029).label().contains("HLock"));
    }
}
