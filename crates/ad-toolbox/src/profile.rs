//! One coherent description of the machine modules believe they are running on.
//!
//! # Why this exists
//!
//! Before this, capabilities were scattered literals that contradicted the
//! runtime. `Gestalt` answered `proc = 3` (a 68030) with an FPU and a PMMU while
//! [`ad_m68k::CpuType::M68000`] was selected, the exception frame was the
//! 68000's six bytes, and addresses were masked to 24 bits. `Gestalt` also
//! advertised multichannel 16-bit sound while every decoded sound is folded to
//! 8-bit mono. A module that believes those claims can take a code path the
//! runtime cannot honour, and the failure surfaces far from the lie.
//!
//! # Honesty is measured, not assumed
//!
//! Reporting less is not automatically safer. Telling modules Color QuickDraw
//! was absent is precisely what made Lunatic Fringe take its Mac Plus path —
//! 1-bit sprites and `x/8` pixel arithmetic — and draw the entire game into the
//! left eighth of the screen. One capability byte cost the whole colour engine.
//!
//! So each field here records **what the runtime can actually do**, and every
//! change to one is gated on the 66-module survey baseline
//! (`tools/lab/survey.py --check`). Where truth would cost compatibility, that
//! tension belongs in a comment naming the module, not in a quiet revert.

/// Which CPU the runtime emulates, and therefore what it may claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cpu {
    M68000,
    M68020,
    M68030,
    M68040,
}

impl Cpu {
    /// `gestaltProcessorType`: 1 = 68000, 2 = 68010, 3 = 68020, 4 = 68030, 5 = 68040.
    ///
    /// Note the off-by-one against the obvious mapping — the selector is a
    /// 1-based enumeration, not a model number. The previous code answered `3`
    /// intending "68030" and actually said **68020**, which is its own small
    /// proof that these values should not be inline literals.
    #[must_use]
    pub const fn gestalt_processor(self) -> u32 {
        match self {
            Self::M68000 => 1,
            Self::M68020 => 3,
            Self::M68030 => 4,
            Self::M68040 => 5,
        }
    }

    /// `SysEnvirons`' `processor` field, which uses the same 1-based encoding.
    #[must_use]
    pub const fn sysenvirons_processor(self) -> i16 {
        match self {
            Self::M68000 => 1,
            Self::M68020 => 3,
            Self::M68030 => 4,
            Self::M68040 => 5,
        }
    }

    /// Address bus width. The 68000 and 68020+ differ, and `ad_memory` masks to
    /// 24 bits, so this must agree with [`ad_memory::ADDRESS_MASK`].
    ///
    /// This deliberately reports **24 for every CPU**. A 68020 has a 32-bit bus,
    /// but `ad_memory` masks addresses regardless, and that mask is load-bearing:
    /// on a 24-bit Macintosh the high byte of a master pointer carries the lock
    /// and purge flags, and dereferencing works only because the bus discards
    /// them. Reporting 32 would invite a module to run 32-bit-clean and hand us
    /// pointers whose top byte means something — the transition that broke a
    /// generation of Mac software. The CPU is raised for its *instruction set*
    /// (see [`Cpu::core_type`]); the address space stays where the memory layer
    /// actually is.
    #[must_use]
    pub const fn address_bits(self) -> u8 {
        24
    }

    /// The Musashi core that executes this CPU's instruction set.
    ///
    /// This pairing is why the field exists. `Gestalt` used to answer from one
    /// constant while [`ad_m68k::Cpu::new`] was handed another, so the machine
    /// could describe itself as something it was not executing as. Now there is
    /// one source of truth and the compiler enforces the match.
    #[must_use]
    pub const fn core_type(self) -> ad_m68k::CpuType {
        match self {
            Self::M68000 => ad_m68k::CpuType::M68000,
            Self::M68020 => ad_m68k::CpuType::M68020,
            Self::M68030 => ad_m68k::CpuType::M68030,
            Self::M68040 => ad_m68k::CpuType::M68040,
        }
    }
}

/// What the sound backend can actually deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundCapabilities {
    pub stereo: bool,
    pub sixteen_bit: bool,
    pub multichannel: bool,
}

impl SoundCapabilities {
    /// What `snd.rs` decodes today: one channel, eight bits, mono.
    pub const MONO_8_BIT: Self = Self {
        stereo: false,
        sixteen_bit: false,
        multichannel: false,
    };

    /// `gestaltSoundAttr` bits.
    ///
    /// `SoundIOMgrPresent` (bit 2) is reported unconditionally because the
    /// Sound Manager traps *are* implemented; it describes the API's presence,
    /// not the hardware's fidelity. The bits that describe fidelity follow the
    /// fields.
    #[must_use]
    pub const fn gestalt_bits(self) -> u32 {
        let mut bits = 1 << 2; // gestaltSoundIOMgrPresent
        if self.stereo {
            bits |= 1 << 0; // gestaltStereoCapability
            bits |= 1 << 1; // gestaltStereoMixing
        }
        if self.sixteen_bit {
            bits |= 1 << 6; // gestalt16BitSoundIO
            bits |= 1 << 11; // gestalt16BitAudioSupport
        }
        if self.multichannel {
            bits |= 1 << 10; // gestaltMultiChannels
        }
        bits
    }
}

/// One attached display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Display {
    pub width: u16,
    pub height: u16,
    /// Bits per pixel. Only 8 is implemented by the blitter's screen path.
    pub depth: u8,
}

impl Display {
    /// Bytes per scanline. Chunky 8-bit, so one byte per pixel.
    #[must_use]
    pub const fn row_bytes(self) -> u32 {
        (self.width as u32 * self.depth as u32).div_ceil(8)
    }
}

/// The machine, as one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineProfile {
    pub cpu: Cpu,
    /// `gestaltSystemVersion` / `SysEnvirons`' `systemVersion`, BCD.
    pub system_version: u16,
    pub color_quickdraw: bool,
    pub fpu: bool,
    pub mmu: bool,
    pub ram_bytes: u32,
    /// Processor clock in hertz.
    ///
    /// This is not decoration: emulated ticks advance from executed cycles, so
    /// `clock_hz / 60` is the cycle budget for one 60th of a second — and once
    /// something paces those ticks against a real clock (`ad_runtime::Pacer` in
    /// the player), this *is* how fast modules run.
    ///
    /// # Unlike every other field here, this is a preference, not a fact
    ///
    /// The rest of this struct records what the runtime can do. This one has no
    /// single right answer, because the original had none: After Dark 2.0x ran on
    /// everything from an 8 MHz Mac Plus to a 40 MHz Quadra, and the 3.0
    /// Programmer's Manual says the call rate "depends on how loaded down the
    /// system is". A module's animation speed was whatever your machine gave it.
    ///
    /// Measured across the 66: raising this from 8 MHz to a Macintosh LC's real
    /// 15.6672 MHz changed ten modules and changed them **both ways** — Mountains
    /// drew twice as much, Strange Attractors dropped from 563 pixels to 4. So it
    /// is a knob with visible consequences and no way to settle it from the
    /// binaries alone; the oracle is what would adjudicate. Until then it stays
    /// where the survey baseline was measured, and the player exposes it as
    /// `AD_MHZ` so it can be judged by eye.
    pub clock_hz: u32,
    pub displays: Vec<Display>,
    pub sound: SoundCapabilities,
}

impl Default for MachineProfile {
    fn default() -> Self {
        Self::honest()
    }
}

impl MachineProfile {
    /// What this runtime genuinely is.
    ///
    /// Per-field rationale, since each of these was measured against the
    /// 66-module baseline rather than reasoned about:
    ///
    /// * `cpu: M68020` — **raised from `M68000`**, because Mandelbrot's
    ///   fixed-point inner loop emits `MULS.L` (`$4C02`) and `DIVS.L`, the
    ///   68020's 32×32→64 multiply and divide. There is no `Gestalt` or
    ///   `SysEnvirons` call anywhere in that module: it does not test the
    ///   processor, it simply requires one, and on a Mac Plus it would have
    ///   crashed exactly as this runtime did. So this is a fact about the
    ///   module, not a concession — and a Macintosh II-class machine is the
    ///   representative After Dark 2.0x host anyway, not a Plus.
    ///
    ///   `M68020` specifically, not `M68030`: the '030 has an integrated PMMU,
    ///   so claiming one while `mmu: false` would be an incoherent machine. A
    ///   68020 with neither FPU nor PMMU is a real configuration — that is a
    ///   Macintosh LC — and it is the one this runtime actually implements.
    ///
    ///   Measured: 0 of 66 modules regressed, Mandelbrot went from an illegal
    ///   instruction to 20 frames in 86 colours, and Mountains' ink rose. The
    ///   first attempt *did* regress 54 modules, because the trap gate assumed
    ///   the 68000's six-byte exception frame and every later CPU appends a
    ///   format word; see [`ad_m68k::CpuType::exception_frame_size`].
    /// * `system_version: 0x0752` — **kept**. Modules gate features on the
    ///   system version and the source disk is a 7.x-era product; lowering it
    ///   is not "honesty" about this runtime, which implements 7.x behaviour.
    /// * `color_quickdraw: true` — genuinely supported, and clearing it
    ///   demonstrably breaks modules (see the module docs).
    /// * `fpu: false` — no FPU is emulated. Saying so is *safer*: modules then
    ///   route floating point through SANE, which is implemented.
    /// * `mmu: false` — addressing is 24-bit. Claiming a PMMU invites a module
    ///   to switch to 32-bit mode and use addresses the mask would silently
    ///   truncate. Lunatic Fringe calls `_SwapMMUMode` over a thousand times a
    ///   session and works only because every address it touches happens to fit.
    /// * `sound: MONO_8_BIT` — what `snd.rs` actually produces.
    /// * `clock_hz: 8_000_000` — where the survey baseline was measured, and
    ///   **not** a claim to be a real 8 MHz machine: a 68020 Mac at 8 MHz never
    ///   existed. Unlike the fields above this one has no correct value, because
    ///   the original ran at every speed from a Mac Plus to a Quadra. Trying the
    ///   Macintosh LC's real 15.6672 MHz moved ten modules in both directions,
    ///   so it is not a free correction. See the field's own documentation.
    #[must_use]
    pub fn honest() -> Self {
        Self {
            cpu: Cpu::M68020,
            system_version: 0x0752,
            color_quickdraw: true,
            fpu: false,
            mmu: false,
            ram_bytes: ad_memory::RAM_SIZE,
            clock_hz: 8_000_000,
            displays: vec![Display {
                width: 640,
                height: 480,
                depth: 8,
            }],
            sound: SoundCapabilities::MONO_8_BIT,
        }
    }

    /// The main display, which every module reads bounds and depth from.
    ///
    /// # Panics
    /// If the profile has no displays; a machine with no screen cannot run a
    /// screen saver, so this is a construction error rather than a runtime case.
    #[must_use]
    pub fn main_display(&self) -> Display {
        *self
            .displays
            .first()
            .expect("a MachineProfile needs at least one display")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_honest_profile_agrees_with_the_runtime_it_describes() {
        let p = MachineProfile::honest();
        // The whole point of the type: these can no longer drift apart silently.
        assert_eq!(p.cpu, Cpu::M68020, "Mandelbrot needs MULS.L; see `honest`");
        assert_eq!(
            p.cpu.core_type(),
            ad_m68k::CpuType::M68020,
            "the claim and the executing core are one field now"
        );
        // Addressing does not follow the CPU up: `ad_memory` masks to 24 bits,
        // and master-pointer flag bits depend on that.
        assert_eq!(
            u32::from(p.cpu.address_bits()),
            24,
            "must match ad_memory::ADDRESS_MASK"
        );
        assert_eq!(ad_memory::ADDRESS_MASK, 0x00FF_FFFF);
        assert!(!p.fpu, "no FPU is emulated; SANE handles floating point");
        // A 68020 with no PMMU is a Macintosh LC. A 68030 would have one
        // integrated, so `M68030` here with `mmu: false` would be incoherent.
        assert!(!p.mmu, "addressing is 24-bit");
        assert!(
            p.color_quickdraw,
            "Color QuickDraw is genuinely implemented"
        );
        assert_eq!(p.sound, SoundCapabilities::MONO_8_BIT);
    }

    #[test]
    fn processor_selectors_use_the_1_based_encoding() {
        // gestaltProcessorType is an enumeration, not a model number. The old
        // inline `3` was meant as "68030" and actually meant 68020.
        assert_eq!(Cpu::M68000.gestalt_processor(), 1);
        assert_eq!(Cpu::M68020.gestalt_processor(), 3);
        assert_eq!(Cpu::M68030.gestalt_processor(), 4);
        assert_eq!(Cpu::M68040.gestalt_processor(), 5);
    }

    #[test]
    fn mono_8_bit_sound_advertises_the_api_but_not_fidelity_it_lacks() {
        let bits = SoundCapabilities::MONO_8_BIT.gestalt_bits();
        assert_eq!(bits & (1 << 2), 1 << 2, "the Sound Manager traps do exist");
        for (bit, what) in [
            (0, "stereo"),
            (1, "stereo mixing"),
            (6, "16-bit sound IO"),
            (10, "multichannel"),
            (11, "16-bit audio"),
        ] {
            assert_eq!(bits & (1 << bit), 0, "must not claim {what}");
        }
    }

    #[test]
    fn a_stereo_16_bit_profile_would_set_the_matching_bits() {
        // Guards the encoder itself, so the day a real mixer lands the bits are
        // already known-correct rather than newly guessed.
        let caps = SoundCapabilities {
            stereo: true,
            sixteen_bit: true,
            multichannel: true,
        };
        let bits = caps.gestalt_bits();
        for bit in [0, 1, 2, 6, 10, 11] {
            assert_ne!(bits & (1 << bit), 0, "bit {bit} should be set");
        }
    }

    #[test]
    fn display_row_bytes_matches_the_screen_the_blitter_assumes() {
        let d = MachineProfile::honest().main_display();
        assert_eq!(
            d.row_bytes(),
            u32::from(d.width),
            "8bpp is one byte a pixel"
        );
        assert_eq!(d.row_bytes(), crate::quickdraw::SCREEN_ROW_BYTES);
        assert_eq!(d.width, crate::quickdraw::SCREEN_WIDTH);
        assert_eq!(d.height, crate::quickdraw::SCREEN_HEIGHT);
    }
}
