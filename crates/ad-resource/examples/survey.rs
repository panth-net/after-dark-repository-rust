//! Survey a directory of `.rsrc` files with the Rust parser.
//!
//! Cross-validation instrument: the numbers this prints must match those from the
//! independent Python implementation in `tools/audit/`. Two implementations
//! agreeing is how parser bugs get caught; one implementation agreeing with
//! itself proves nothing.
//!
//! ```text
//! cargo run -p ad-resource --example survey -- <dir-of-rsrc-files>
//! ```

#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use ad_resource::{AdModule, ModuleSettings, ResourceFork, TYPE_ADGM};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: survey <dir-of-rsrc-files>")?,
    );

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "rsrc"))
        .collect();
    paths.sort();

    let mut modules = 0usize;
    let mut parse_failures: Vec<(String, String)> = Vec::new();
    let mut with_header = 0usize;
    let mut id_matches = 0usize;
    let mut stub_resolved = 0usize;
    let mut bare = 0usize;
    let mut unresolved: Vec<String> = Vec::new();
    let mut with_segments = 0usize;
    let mut broken_chains: Vec<String> = Vec::new();
    let mut with_sound = 0usize;
    let mut type_hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_resources = 0usize;
    let mut total_code = 0usize;
    let mut off_spec_sysz: Vec<String> = Vec::new();
    let mut buttons_seen: Vec<(String, i16, String)> = Vec::new();

    for path in &paths {
        let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let bytes = std::fs::read(path)?;
        let fork = match ResourceFork::parse(&bytes) {
            Ok(f) => f,
            Err(e) => {
                parse_failures.push((name, e.to_string()));
                continue;
            }
        };
        total_resources += fork.len();
        for r in fork.all() {
            *type_hist.entry(r.type_str()).or_default() += 1;
        }

        if fork.count_of(&TYPE_ADGM) == 0 {
            continue; // not a graphics module (control panel, settings file, ...)
        }
        modules += 1;

        let module = AdModule::new(fork);
        let code = module.code().expect("counted above");
        total_code += code.data.len();
        let layout = module.code_layout().expect("has code");

        match layout.header {
            Some(h) => {
                with_header += 1;
                if h.declared_id == code.id {
                    id_matches += 1;
                }
                if layout.resolved_via_stub {
                    stub_resolved += 1;
                } else {
                    unresolved.push(name.clone());
                }
            }
            None => bare += 1,
        }

        let segs = module.segments();
        if !segs.is_empty() {
            with_segments += 1;
            if let Err((id, got, want)) = module.verify_segment_chain() {
                broken_chains.push(format!("{name}: CCOD {id} declared {got}, expected {want}"));
            }
        }

        let settings = ModuleSettings::from_fork(module.fork());
        if settings.sound.is_some() {
            with_sound += 1;
        }
        for (msg, label) in settings.buttons() {
            buttons_seen.push((name.clone(), msg, label.unwrap_or("<unnamed>").to_string()));
        }
        if !settings.memory.off_spec.is_empty() {
            off_spec_sysz.push(format!("{name}: {:?}", settings.memory.off_spec));
        }
    }

    println!("files scanned            {}", paths.len());
    println!("parse failures           {}", parse_failures.len());
    for (n, e) in &parse_failures {
        println!("    !! {n}: {e}");
    }
    println!("total resources parsed   {total_resources}");
    println!();
    println!("ADgm modules             {modules}");
    println!("  with 16-byte header    {with_header}");
    println!("    header id == res id  {id_matches}/{with_header}");
    println!("    entry via stub       {stub_resolved}/{with_header}");
    println!("    entry UNRESOLVED     {}", unresolved.len());
    for n in &unresolved {
        println!("        - {n}");
    }
    println!("  bare (entry at 0)      {bare}");
    println!("  with CCOD segments     {with_segments}");
    println!("    broken chains        {}", broken_chains.len());
    for b in &broken_chains {
        println!("        !! {b}");
    }
    println!("  with Chnl sound config {with_sound}");
    println!("  total ADgm code bytes  {total_code}");
    println!();
    println!("off-spec sysz ids        {}", off_spec_sysz.len());
    for s in &off_spec_sysz {
        println!("    {s}");
    }
    println!();
    println!("settings buttons found   {}", buttons_seen.len());
    for (m, msg, label) in buttons_seen.iter().take(12) {
        println!("    {m:<28} message {msg:>3}  {label:?}");
    }
    println!();
    println!("distinct resource types  {}", type_hist.len());
    let mut hist: Vec<_> = type_hist.iter().collect();
    hist.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (ty, n) in hist.iter().take(18) {
        println!("    {ty:?} {n}");
    }
    Ok(())
}
