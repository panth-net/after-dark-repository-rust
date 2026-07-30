//! Expanding System 7 compressed resources by running the module's own `dcmp`.
//!
//! # Why this exists
//!
//! Every module on the After Dark 3.0-era discs — the 3.0 set, Totally Twisted,
//! and the After Dark Classic additions including Rat Race — stores its code
//! and art as **compressed resources**. A compressed resource begins with the
//! long `0xA89F6572`, whose first word `$A89F` is also the *Unimplemented*
//! A-line trap. Hand those bytes to a CPU and the very first instruction is a
//! trap nothing implements, which is why seventeen perfectly sound modules all
//! reported "unhandled Toolbox trap $A89F at PC 0x008000" and looked like they
//! needed some missing After Dark engine service. They did not. They needed
//! unpacking.
//!
//! # Why run the decompressor rather than reimplement it
//!
//! The header names a `dcmp` resource to expand it, and these modules carry
//! their own — `dcmp 128`, a byte-identical 444-byte third-party decompressor
//! in every one of them, not one of the System's `dcmp 0/1/2`. Reimplementing
//! a proprietary format from its compiled code would be guesswork with a
//! failure mode of *plausible-looking garbage*. This project's whole premise is
//! that the original code is the specification, so the `dcmp` is executed on
//! the same 68K core the modules themselves run on.
//!
//! # The interface
//!
//! From Apple's published `dcmp` glue (MacTech vol. 9 no. 1, "Resource
//! Compression"), a decompressor is entered with a stack frame of four
//! longs and lifts them into registers:
//!
//! ```text
//!     link    a6,#0
//!     movem.l d0-d7/a0-a6,-(a7)
//!     move.l  20(a6),a0     ; sourceBuffer
//!     move.l  16(a6),a1     ; destinationBuffer
//!     move.l  12(a6),a2     ; workingBuffer
//!     move.l   8(a6),d0     ; dataSize
//! ```
//!
//! so the caller pushes `sourceBuffer`, `destinationBuffer`, `workingBuffer`,
//! `dataSize` in that order and the frame reads back at `8(a6)`..`20(a6)`.
//! Registers are set here as well as the stack: this `dcmp 128` jumps straight
//! to `movem.l` without a `link`, so it takes its arguments from registers
//! rather than from the frame, and satisfying both conventions costs nothing.
//!
//! The entry point is **not** the start of the resource. A `dcmp` opens with a
//! small table of word offsets; `dcmp 128`'s reads `000A 000E 000A 0001`, where
//! `+0x0A` is a `MOVE.L (A7)+,(A7); RTS` shim and `+0x0E` is the real routine.
//! The second word is used as the entry, with a fallback for a malformed table.
//!
//! # Status: runs faithfully, produces nothing — and the reason is now known
//!
//! This drives `dcmp 128` through ~5,200 cycles of genuine execution to a clean
//! return, and it writes **nothing** — not to the destination, not to the
//! working buffer, not anywhere in the scratch span (checked by scanning it;
//! see `AD_DCMP_DEBUG`). The argument convention is not the problem, and
//! neither is the entry point. The decompressor is *obfuscated*.
//!
//! Traced, its real entry (`+0x0E`) saves the register file and branches to
//! `+0x146`, which begins:
//!
//! ```text
//!     lea     -0x132(pc),a4       ; recover its own base
//!     movea.l a4,a3
//!     move.w  -0x14(pc),d5        ; a word out of its own header
//!     move.w  #2,-2(a3)           ; write into itself
//!     moveq   #$3f,d0
//!     ror.b   #1,d0               ; d0 = $9f
//!     _GetToolTrapAddress         ; $A746 — address of trap $A89F
//!     movea.l a0,a6
//!     moveq   #$7b,d0
//!     ror.b   #1,d0               ; d0 = $bd
//!     _GetOSTrapAddress           ; $A346
//!     movea.l a0,a5
//!     lea     0x10(pc),a0
//!     neg.l   (a0)                ; …and rewrites its own code
//!     not.l   -(a0)
//! ```
//!
//! Three things follow. It **modifies its own code** (`neg.l`/`not.l` over
//! nearby instructions) — this body is encrypted and decrypts itself at
//! runtime. It seeds that from the **trap table**, taking the addresses of
//! `$A89F` and an OS trap as inputs. And this runtime answers
//! `_GetTrapAddress` with *synthetic* addresses — deliberately
//! distinguishable, deliberately not real ROM (see the `$A046` arm in
//! `ad_toolbox`) — so the decryption is keyed on values that are wrong here.
//! The tight loops the trace shows afterwards are that mis-decrypted body,
//! running harmlessly and returning.
//!
//! **So running the shipped decompressor cannot work without a real trap table
//! — that is, without ROM addresses this project does not have and does not
//! want to depend on.** Two honest routes remain, both real work:
//!
//! 1. Make the trap-address answers indistinguishable from a real machine's for
//!    the two traps it probes, and see whether the decryption comes out. Cheap
//!    to try, and the plausibility check below makes a wrong outcome loud.
//! 2. Recover the algorithm from the *decrypted* body — dump it after
//!    self-modification under a correct key, then reimplement it natively.
//!
//! The check on the result stays either way: a wrong key produces
//! plausible-looking garbage, and writing that into a module's code region
//! would fail somewhere far away from here.

use ad_resource::fork::{Compression, Resource};
use ad_toolbox::Toolbox;

/// Bytes of header before the compressed payload. Always 18; the header's own
/// length field says so and is checked.
const HEADER_LEN: u32 = 18;

/// Which **word** of the leading table holds the decompressor's entry offset.
///
/// `dcmp 128`'s table reads `000A 000E 000A 0001`. Word 0 (`+0x0A`) is a
/// `MOVE.L (A7)+,(A7); RTS` shim that returns having done nothing — taking it
/// for the entry point produced a returned call and an untouched destination
/// buffer, which is exactly what "the decompressor produced an entirely blank
/// buffer" was reporting. Word 1 (`+0x0E`) is the real routine.
const ENTRY_SLOT: usize = 1;

/// Fallback entry offset if the table is not readable.
const DEFAULT_ENTRY: u32 = 0x0E;

/// Extra room for the working buffer, over and above the expanded size.
///
/// The type 8 header carries a working-buffer fraction and an expansion-buffer
/// size; the type 9 header used here does not, so the decompressor is simply
/// given generous scratch. Cheap, and a decompressor that overruns a tight
/// buffer would corrupt the emulated heap in a way that is very hard to trace.
const WORKING_SLACK: u32 = 0x4000;

/// Why an expansion could not be performed or could not be trusted.
#[derive(Debug)]
pub enum DecompressError {
    /// The module names a `dcmp` it does not carry, and the System ones this
    /// runtime does not supply.
    MissingDecompressor(i16),
    /// The decompressor faulted, hung, or ran past its budget.
    Ran(String),
    /// It returned, but the result cannot be what was compressed.
    ///
    /// Checked rather than assumed: a decompressor fed the wrong ABI usually
    /// *returns*, having written something. Silently accepting it would put
    /// nonsense where the module's code should be and produce a failure
    /// somewhere else entirely.
    Implausible(String),
}

impl std::fmt::Display for DecompressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDecompressor(id) => {
                write!(f, "resource needs dcmp {id}, which the module does not carry")
            }
            Self::Ran(e) => write!(f, "the decompressor failed: {e}"),
            Self::Implausible(e) => write!(f, "the decompressor produced {e}"),
        }
    }
}

/// Cycles a single expansion may take. Decompressing tens of kilobytes is a
/// tight loop over the input; this is orders of magnitude more than needed and
/// exists so a wrong entry point ends as an error rather than a hang.
const BUDGET: u32 = 200_000_000;

/// Expand `res` by running `dcmp_code` on `cpu`, returning the plain bytes.
///
/// `scratch` is emulated memory the expansion may use: it must hold the source,
/// the destination and the working buffer. Nothing outside it is written.
///
/// # Errors
/// [`DecompressError`], including when the output fails its plausibility check.
pub fn expand(
    tb: &mut Toolbox,
    cpu: &mut ad_m68k::Cpu,
    header: Compression,
    packed: &[u8],
    dcmp_code: &[u8],
    scratch: u32,
) -> Result<Vec<u8>, DecompressError> {
    let payload = packed
        .get(HEADER_LEN as usize..)
        .ok_or_else(|| DecompressError::Implausible("a truncated header".to_owned()))?;

    // Lay the three buffers out in scratch, each on an even address: a 68000
    // faults on an odd word access, and a decompressor writing words to an odd
    // destination would fail in a way that looks like a bad algorithm.
    let src = scratch;
    let src_len = u32::try_from(payload.len()).unwrap_or(0);
    let dst = align(src + src_len + 0x10);
    let work = align(dst + header.unpacked_len + 0x10);
    let dcmp_at = align(work + header.unpacked_len + WORKING_SLACK);
    let stack = align(dcmp_at + u32::try_from(dcmp_code.len()).unwrap_or(0) + 0x100) + 0x2000;

    tb.mem.write_bytes(src, payload);
    tb.mem.write_bytes(dcmp_at, dcmp_code);
    // The destination is cleared so a decompressor that writes less than it
    // promised cannot leave previous contents to be mistaken for output.
    for i in 0..header.unpacked_len {
        tb.mem.write_u8(dst.wrapping_add(i), 0);
    }

    let entry = dcmp_at.wrapping_add(entry_offset(dcmp_code));

    // The Pascal-style frame the published glue reads: pushed so that
    // `dataSize` lands at 8(a6) and `sourceBuffer` at 20(a6).
    let mut sp = stack;
    let mut push = |v: u32, mem: &mut ad_memory::Memory| {
        sp -= 4;
        mem.write_u32(sp, v);
    };
    push(src, &mut tb.mem);
    push(dst, &mut tb.mem);
    push(work, &mut tb.mem);
    push(src_len, &mut tb.mem);
    // Return address: the host sentinel, so reaching it ends the run.
    sp -= 4;
    tb.mem.write_u32(sp, crate::HOST_RETURN);

    cpu.reset(tb);
    cpu.set_stop_address(Some(crate::HOST_RETURN));
    // Wild-jump bounds off: the decompressor legitimately runs outside the
    // module code region, which is what those bounds exist to catch.
    cpu.set_wild_jump_floor(0);
    cpu.set_wild_jump_ceiling(0);
    cpu.set_sp(sp);
    cpu.set_pc(entry);
    // Both conventions at once; see the module docs.
    cpu.set_addr(0, src);
    cpu.set_addr(1, dst);
    cpu.set_addr(2, work);
    cpu.set_data(0, src_len);

    let debug = std::env::var_os("AD_DCMP_DEBUG").is_some();
    if debug {
        cpu.set_trace(4096);
    }
    let mut spent = 0u32;
    loop {
        let ran = cpu
            .run(tb, 1_000_000)
            .map_err(|e| DecompressError::Ran(e.to_string()))?;
        let hit = cpu.take_stop_hit();
        if debug {
            let t = cpu.trace();
            eprintln!("[dcmp] ran {ran} cycles, stop_hit={hit}, {} traced", t.len());
            // As offsets into the dcmp, which is what the disassembly is in.
            let off: Vec<String> = t
                .iter()
                .map(|pc| {
                    if *pc >= dcmp_at && *pc < dcmp_at + 0x400 {
                        format!("+{:x}", pc - dcmp_at)
                    } else {
                        format!("{pc:#x}")
                    }
                })
                .collect();
            eprintln!("[dcmp] first 40: {}", off.iter().take(40).cloned().collect::<Vec<_>>().join(" "));
            eprintln!("[dcmp] last 40:  {}", off.iter().rev().take(40).rev().cloned().collect::<Vec<_>>().join(" "));
        }
        if hit {
            break;
        }
        spent = spent.saturating_add(ran.max(1));
        if spent >= BUDGET {
            return Err(DecompressError::Ran(format!(
                "no return after {spent} cycles (entry {entry:#x})"
            )));
        }
    }

    let out = tb
        .mem
        .read_bytes(dst, header.unpacked_len as usize);
    if std::env::var_os("AD_DCMP_DEBUG").is_some() {
        // Where did it actually write? Scan the whole scratch span for runs of
        // non-zero bytes; the answer names the buffer it really used.
        let span = header.unpacked_len.saturating_mul(2).saturating_add(0x2_0000);
        let mut run_start: Option<u32> = None;
        let mut reported = 0;
        for a in scratch..scratch.saturating_add(span) {
            let nz = tb.mem.read_u8(a) != 0;
            match (nz, run_start) {
                (true, None) => run_start = Some(a),
                (false, Some(st)) => {
                    if a - st > 8 && reported < 12 {
                        let what = if st >= src && st < src + src_len { "SRC" }
                            else if st >= dst && st < dst + header.unpacked_len { "DST" }
                            else if st >= work && st < dcmp_at { "WORK" }
                            else if st >= dcmp_at && st < stack { "DCMP" }
                            else { "other" };
                        eprintln!("[dcmp] wrote {} bytes at {st:#x} ({what})", a - st);
                        reported += 1;
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        eprintln!("[dcmp] layout src={src:#x}+{src_len} dst={dst:#x}+{} work={work:#x} dcmp={dcmp_at:#x} stack={stack:#x}",
            header.unpacked_len);
    }
    if std::env::var_os("AD_DCMP_DEBUG").is_some() {
        let nz = out.iter().filter(|b| **b != 0).count();
        eprintln!(
            "[dcmp] entry={entry:#x} (base {dcmp_at:#x} + {:#x}) src={src:#x} len={src_len} \
             dst={dst:#x} want={} nonzero={nz} head={:02x?}",
            entry.wrapping_sub(dcmp_at),
            header.unpacked_len,
            out.get(..16).unwrap_or(&[])
        );
        // Did it write anywhere else in scratch?
        let work_bytes = tb.mem.read_bytes(work, 64);
        eprintln!("[dcmp] work head={:02x?}", &work_bytes[..16.min(work_bytes.len())]);
    }
    if out.iter().all(|b| *b == 0) {
        return Err(DecompressError::Implausible(
            "an entirely blank buffer".to_owned(),
        ));
    }
    Ok(out)
}

/// Round up to an even address.
fn align(a: u32) -> u32 {
    a.wrapping_add(1) & !1
}

/// The decompressor's entry offset, from its leading table of word offsets.
fn entry_offset(dcmp: &[u8]) -> u32 {
    let at = ENTRY_SLOT * 2;
    let word = dcmp
        .get(at..at + 2)
        .and_then(|b| <[u8; 2]>::try_from(b).ok())
        .map(u16::from_be_bytes)
        .unwrap_or(0);
    // A table slot that points outside the resource is not an entry point.
    if word == 0 || usize::from(word) >= dcmp.len() {
        return DEFAULT_ENTRY;
    }
    u32::from(word)
}

/// Whether `res` needs expanding, and its header if so.
#[must_use]
pub fn needed(res: &Resource<'_>) -> Option<Compression> {
    res.compression()
}
