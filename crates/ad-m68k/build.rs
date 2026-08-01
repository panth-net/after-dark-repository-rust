//! Build the vendored Musashi core.
//!
//! Musashi does not ship its opcode dispatch tables: a C program (`m68kmake`)
//! reads `m68k_in.c` and emits `m68kops.c` / `m68kops.h`. Building that
//! generator from a build script is a trap in two directions. `cc` describes
//! only the *target* toolchain, so a cross build compiles a generator the build
//! host cannot execute; and driving a compiler by hand means hard-coding an
//! output flag, which is `-o` on Unix and `/Fe:` under MSVC.
//!
//! The tables are a pure function of `m68k_in.c` — the generator reads no clock,
//! environment, or randomness — so they are generated once and committed under
//! `vendor/generated/`. The normal build therefore compiles target C only: no
//! host tool, no generated binary to execute, identical bytes on every platform.
//! Maintainers who touch `m68k_in.c` or `m68kmake.c` re-derive the tables with
//! `AD_M68K_REGENERATE=1`, which regenerates and fails on any drift from the
//! committed copies rather than silently building something else.
//!
//! What this tree is, and the hash of every byte of it: `vendor/PROVENANCE.md`.

use std::path::{Path, PathBuf};

/// Inputs whose contents can change the compiled core, including the generator
/// sources: they are not compiled here, but they define what the committed
/// tables are supposed to be, so a stale table must show up as a rebuild under
/// `AD_M68K_REGENERATE=1` rather than as a mystery at runtime.
const BUILD_INPUTS: &[&str] = &[
    "m68k.h",
    "m68kconf.h",
    "m68kcpu.c",
    "m68kcpu.h",
    "m68kdasm.c",
    "m68kfpu.c",
    "m68kmmu.h",
    "m68k_in.c",
    "m68kmake.c",
    "softfloat/softfloat.c",
    // SoftFloat's headers carry code, not just declarations: `softfloat.h`
    // includes `softfloat-macros` and `softfloat.c` includes
    // `softfloat-specialize`. Omitting them would let an edit to the FPU
    // arithmetic leave a stale object linked in.
    "softfloat/softfloat.h",
    "softfloat/softfloat-macros",
    "softfloat/softfloat-specialize",
    "softfloat/milieu.h",
    "softfloat/mamesf.h",
    "generated/m68kops.c",
    "generated/m68kops.h",
];

/// The generator's two outputs, named exactly as `m68kmake` writes them.
const GENERATED: &[&str] = &["m68kops.c", "m68kops.h"];

fn main() {
    let vendor = PathBuf::from("vendor");
    let generated = vendor.join("generated");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    for f in BUILD_INPUTS {
        println!("cargo:rerun-if-changed={}", vendor.join(f).display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=AD_M68K_REGENERATE");

    if std::env::var("AD_M68K_REGENERATE").is_ok_and(|v| v == "1") {
        verify_generated_tables(&vendor, &generated, &out);
    }

    // The include graph crosses both directories in both directions:
    // `generated/m68kops.c` includes "m68kcpu.h", and `m68kcpu.c` includes
    // "m68kops.h". Every translation unit therefore needs both on its path.
    cc::Build::new()
        .file(vendor.join("m68kcpu.c"))
        .file(vendor.join("m68kdasm.c"))
        .file(generated.join("m68kops.c"))
        .file(vendor.join("softfloat/softfloat.c"))
        .include(&vendor)
        .include(&generated)
        .warnings(false)
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-shift-negative-value")
        .flag_if_supported("-Wno-implicit-fallthrough")
        .compile("musashi");
}

/// Regenerate the opcode tables into `OUT_DIR` and panic unless they match the
/// committed copies (modulo line endings — see [`read_normalized`]).
///
/// Opt-in only. This is the one code path that needs a working host C compiler
/// and the ability to execute what it builds, which is exactly what the normal
/// build must not require.
fn verify_generated_tables(vendor: &Path, generated: &Path, out: &Path) {
    let fresh = out.join("regenerated");
    std::fs::create_dir_all(&fresh).expect("create regeneration directory");

    let exe = build_host_generator(vendor, out);

    // Argument order is <output dir> <m68k_in.c>; without the first argument the
    // generator writes into the current directory, i.e. into the source tree.
    let status = std::process::Command::new(&exe)
        .arg(&fresh)
        .arg(vendor.join("m68k_in.c"))
        .status()
        .expect("failed to run m68kmake");
    assert!(
        status.success(),
        "m68kmake failed to generate opcode tables"
    );

    for name in GENERATED {
        let committed = generated.join(name);
        let regenerated = fresh.join(name);
        let want = read_normalized(&committed);
        let got = read_normalized(&regenerated);
        assert!(
            want == got,
            "{} does not match the committed copy at {}.\n\
             The vendored generator inputs and the committed tables have drifted \
             apart. Review the regenerated file at {} and commit it if the change \
             is intended.",
            name,
            committed.display(),
            regenerated.display()
        );
    }
}

/// Compile `m68kmake.c` with the **host** compiler and return the executable.
///
/// `cc::Build::new().get_compiler()` would hand back the target compiler, whose
/// output a cross build cannot run. Pinning both `target` and `host` to `HOST`
/// is what makes `cc` resolve the build machine's toolchain instead.
fn build_host_generator(vendor: &Path, out: &Path) -> PathBuf {
    let host = std::env::var("HOST").expect("HOST");
    let compiler = cc::Build::new()
        .target(&host)
        .host(&host)
        // A host-side probe must not contribute link flags to the target crate.
        .cargo_metadata(false)
        .get_compiler();

    let exe = out.join(format!("m68kmake{}", std::env::consts::EXE_SUFFIX));
    let mut cmd = compiler.to_command();
    cmd.arg(vendor.join("m68kmake.c"));
    if compiler.is_like_msvc() {
        // `cl` spells the output flag differently and, left alone, drops the
        // intermediate object into the current directory — the crate root.
        cmd.arg("/nologo");
        cmd.arg(format!("/Fo{}", out.join("m68kmake.obj").display()));
        cmd.arg(format!("/Fe:{}", exe.display()));
    } else {
        cmd.arg("-o").arg(&exe);
    }

    let status = cmd
        .status()
        .expect("failed to invoke the host C compiler for m68kmake");
    assert!(status.success(), "compiling m68kmake failed");
    exe
}

/// Read a generated file with line endings normalised to `\n`.
///
/// `m68kmake` opens its outputs in text mode, so the same logical output is
/// CRLF on Windows and LF everywhere else. Comparing raw bytes would report
/// drift on Windows for tables that are in fact identical.
fn read_normalized(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied().peekable();
    while let Some(b) = iter.next() {
        if b == b'\r' && iter.peek() == Some(&b'\n') {
            continue;
        }
        out.push(b);
    }
    out
}
