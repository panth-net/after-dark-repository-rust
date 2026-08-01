//! Run a real After Dark module headlessly and report what happened.
//!
//! ```text
//! cargo run -p ad-host-v2 --example run_module -- <module.rsrc> [frames] [out.png]
//! ```
//!
//! Prints the module's title, the lifecycle results, the trap log, memory
//! accounting, and any faults — i.e. the per-module row the compatibility lab
//! is built from.

#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use ad_host_v2::Host;
use ad_resource::{AdModule, GmMessage, ModuleSettings, ResourceFork};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or("usage: run_module <module.rsrc> [frames] [out.png]")?,
    );
    let frames: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    let out_png = args.next().map(PathBuf::from);

    let bytes = std::fs::read(&path)?;
    let fork = ResourceFork::parse(&bytes)?;

    // Report what we are about to run before running it, so a hang still leaves
    // useful output.
    {
        let module = AdModule::new(ResourceFork::parse(&bytes)?);
        println!(
            "module    {}",
            path.file_stem().unwrap_or_default().to_string_lossy()
        );
        println!("title     {:?}", module.title());
        if let Some(code) = module.code() {
            println!("code      ADgm {} ({} bytes)", code.id, code.data.len());
        }
        if let Some(l) = module.code_layout() {
            println!(
                "entry     +{:#x} (header {}, stub {})",
                l.entry_offset,
                if l.header.is_some() { "yes" } else { "no" },
                if l.resolved_via_stub {
                    "resolved"
                } else {
                    "fallback"
                }
            );
        }
        let segs = module.segments();
        if !segs.is_empty() {
            println!("segments  {} CCOD", segs.len());
        }
    }

    let settings = ModuleSettings::from_fork(&fork);
    let controls = settings.control_values();
    println!("controls  {controls:?}");
    for (i, c) in settings.controls.iter().enumerate() {
        if let Some(c) = c {
            println!("            [{i}] {c:?}");
        }
    }
    println!();

    // Every switch in one typed value, resolved in exactly one place. Saves are
    // opt-in via `AD_SAVE_DIR`: the 66-module survey must not write into the
    // user's Application Support folder, both because it pollutes and because a
    // second run would then start from the first run's saved state.
    let options = ad_runtime::RuntimeOptions::from_env();

    let mut host = Host::load(fork, controls)?;
    host.set_diagnostics(options.diagnostics);
    println!("layout    {}", host.layout());
    if std::env::var_os("AD_TRACE").is_some() {
        host.set_trace(65536);
    }
    // AD_BUDGET=<millions of cycles per message>. Games that own the machine
    // (Lunatic Fringe in play mode never returns from DrawFrame) need room to
    // actually play before the lab calls them hung.
    if let Some(hz) = options.clock_hz {
        host.tb.profile.clock_hz = hz;
    }
    if let Some(budget) = options.cycle_budget {
        host.cycle_budget = budget;
    }
    // Fonts, from beside the module. `_DrawString` draws nothing without them,
    // and `GetFontInfo` has to answer before Initialize lays anything out.
    {
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        let mut strikes = 0;
        for fork in ad_runtime::font_forks(dir) {
            strikes += host.add_font_fork(&fork);
        }
        println!("fonts     {strikes} strike(s)");
    }

    // Saved state must be merged *before* Initialize: a module reads its high
    // scores there, so a later merge would show the shipped defaults for a whole
    // session and then save those over the real ones.
    // Keyed on the filename, not the `ADrk 0` descriptor — five modules on the
    // original disk share one descriptor, so keying on it makes them collide on
    // a single save file. Matches `ad-player`'s `run_module`.
    let title = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if let Some(dir) = options.save_dir.as_deref() {
        let saved =
            ad_runtime::ForkSink::load(dir, &title).map_err(|e| format!("saved state: {e}"))?;
        println!(
            "saves     {} ({} resource(s) restored)",
            dir.display(),
            saved.len()
        );
        host.attach_saved_state(saved, Box::new(ad_runtime::ForkSink::new(dir, &title)));
    }

    // Initialize.
    match host.call(GmMessage::Initialize) {
        Ok(ad_resource::GmResult::Ok) => println!("Initialize  -> Ok"),
        Ok(r) => {
            // Calling Blank after a failed Initialize runs the module against
            // storage it never allocated; it then crashes, and the run looks like
            // a hang instead of a clean refusal.
            println!("Initialize  -> {r:?}");
            if let Some(m) = host.error_message() {
                println!("            module says: {m:?}");
            }
            let tail = host.trace_tail(40);
            if !tail.is_empty() {
                println!("last instructions before the refusal:\n{tail}");
            }
            report(&host, 0);
            return Err("module declined to initialize".into());
        }
        Err(e) => {
            let tail = host.trace_tail(30);
            if !tail.is_empty() {
                println!("last instructions before the fault:\n{tail}");
            }
            report(&host, 0);
            return Err(format!("Initialize failed: {e}").into());
        }
    }
    println!("debug       {}", host.storage_debug());
    println!(
        "storage     handle {:#x} magic {:?}",
        host.storage(),
        host.storage_magic()
            .map(|m| String::from_utf8_lossy(&m).into_owned())
    );

    // Blank.
    //
    // A module that *declines* Blank must not then be sent DrawFrame. PICS Player
    // returns ModuleError here because it has no picture file, and driving it
    // anyway ran it against state it never built: it spun for 50 million cycles
    // and was filed as a hang for three sessions. This is the same guard already
    // applied to a failed Initialize, and for the same reason.
    match host.call(GmMessage::Blank) {
        Ok(ad_resource::GmResult::Ok) => println!("Blank       -> Ok"),
        Ok(r) => {
            println!("Blank       -> {r:?}");
            if let Some(m) = host.error_message() {
                println!("            module says: {m:?}");
            }
            report(&host, 0);
            return Err("module declined to blank the screen".into());
        }
        Err(e) => {
            report(&host, 0);
            return Err(format!("Blank failed: {e}").into());
        }
    }

    // The emulated clock after Initialize and Blank. Not decoration: an
    // interactive host's pacer anchors here, and anchoring it at zero instead
    // made the first presented frame sleep for `ticks / 60` seconds.
    println!("clock       tick {} after Initialize+Blank", host.tb.ticks);

    // DrawFrame, repeatedly.
    let qd_log = options.diagnostics.qd_log;
    // AD_KEYS="60:39+,t2400:5b+" — at frame 60 press virtual key 0x39 (Caps
    // Lock); at TICK 2400 press 0x5b. Key codes in hex, '+' down, '-' up.
    // Tick entries ('t' prefix) are queued into the host and fire even while
    // a game holds the CPU inside one DrawFrame; frame entries fire between
    // frames. Caps Lock is how a user tells an interactive module to play.
    let mut key_script: Vec<(u32, u8, bool)> = Vec::new();
    if let Ok(s) = std::env::var("AD_KEYS") {
        for part in s.split(',') {
            let Some((when, rest)) = part.split_once(':') else {
                continue;
            };
            let down = rest.ends_with('+');
            let Ok(code) = u8::from_str_radix(rest.trim_end_matches(['+', '-']), 16) else {
                continue;
            };
            if let Some(tick) = when.strip_prefix('t') {
                if let Ok(t) = tick.parse() {
                    println!(
                        "key         {code:#04x} {} at tick {t}",
                        if down { "down" } else { "up" }
                    );
                    host.queue_key(t, code, down);
                }
            } else if let Ok(frame) = when.parse() {
                key_script.push((frame, code, down));
            }
        }
    }
    let png_dir = std::env::var_os("AD_PNG_DIR").map(PathBuf::from);
    if let Some(d) = png_dir.as_ref() {
        std::fs::create_dir_all(d)?;
    }
    let png_every: u32 = std::env::var("AD_PNG_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if let (Some(dir), true) = (png_dir.clone(), png_every > 0) {
        // Presented from inside the tick loop, so a game that never returns
        // from DrawFrame still yields a frame sequence.
        host.set_present_hook(
            png_every,
            Box::new(move |fb, ticks| {
                let p = dir.join(format!("t{ticks:06}.png"));
                let _ = write_png(&p, fb.width, fb.height, &fb.to_rgb());
            }),
        );
    }
    // AD_TEXT_LOG=1 prints every string the module draws, as it draws it.
    //
    // The present hook's counterpart for words. It is how the player's
    // typing-mode detection was checked against the real module: the thing that
    // had to be true was "Lunatic Fringe draws its name-entry prompt *before*
    // the first keystroke", and nothing but running it says whether it does.
    if std::env::var_os("AD_TEXT_LOG").is_some() {
        host.set_text_hook(Box::new(|said| {
            for line in said {
                println!("[text] {line:?}");
            }
        }));
    }
    let mut drawn = 0u32;
    for i in 0..frames {
        for &(frame, code, down) in &key_script {
            if frame == i {
                println!(
                    "key         {code:#04x} {}",
                    if down { "down" } else { "up" }
                );
                host.set_key(code, down);
            }
        }
        if qd_log {
            // Interleaves with the [qd] lines so fills can be tied to frames.
            eprintln!("[frame {i}] ink={}", host.framebuffer().ink());
        }
        // AD_TRACE_FRAME=N: capture and print the PC trace of frame N alone,
        // for seeing what a quiet steady-state frame actually polls.
        let traced = std::env::var("AD_TRACE_FRAME")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        if traced == Some(i) {
            host.set_trace(2048);
        }
        match host.draw_frame() {
            Ok(r) => {
                drawn += 1;
                if traced == Some(i) {
                    eprintln!("[trace frame {i}]\n{}", host.trace_tail(400));
                    host.set_trace(0);
                }
                if r != ad_resource::GmResult::Ok {
                    println!("DrawFrame {i} -> {r:?} (stopping)");
                    break;
                }
            }
            Err(e) => {
                println!("DrawFrame {i} failed: {e}");
                if std::env::var_os("AD_TRACE").is_some() {
                    // For a game looping inside one call, the tail IS the
                    // game's hot loop — the blit math reads right off it.
                    eprintln!("[trace at failure]\n{}", host.trace_tail(60000));
                }
                report(&host, drawn);
                let _ = dump_sounds(&host);
                // The screen at the moment of failure is diagnostic gold:
                // it shows how far the module really got.
                if let Some(p) = out_png {
                    let fb = host.framebuffer();
                    write_png(&p, fb.width, fb.height, &fb.to_rgb())?;
                    println!("\nwrote {} (at failure)", p.display());
                }
                return Err("draw failed".into());
            }
        }
    }
    println!("DrawFrame   -> {drawn}/{frames} frames ok");

    // Measure and snapshot BEFORE Close: modules erase the screen on their way
    // out, and a post-Close reading scored Lunatic Fringe's whole title
    // sequence as "drew nothing".
    let live_ink = host.framebuffer().ink();
    let live_colours = host.framebuffer().distinct();
    if let Some(p) = out_png.as_ref() {
        let fb = host.framebuffer();
        write_png(p, fb.width, fb.height, &fb.to_rgb())?;
        println!("wrote {}", p.display());
    }

    // Close.
    match host.call(GmMessage::Close) {
        Ok(r) => println!("Close       -> {r:?}"),
        Err(e) => println!("Close failed: {e}"),
    }

    println!(
        "\nink (live)  {live_ink} / {}   colours={live_colours}",
        usize::from(host.framebuffer().width) * usize::from(host.framebuffer().height)
    );
    report(&host, drawn);
    dump_sounds(&host)?;
    render_mix(&host)?;
    // `_CloseResFile`: write anything the module changed but never asked to
    // save. Reported rather than swallowed — a lost high score with no
    // explanation is worse than a visible failure.
    if let Err(e) = host.flush_saved_state() {
        eprintln!("saved state not written: {e}");
    }
    Ok(())
}

/// `AD_WAV_DIR=<dir>`: write every sound the module played as a WAV file —
/// the audible equivalent of the PNG snapshot.
fn dump_sounds(host: &Host) -> std::io::Result<()> {
    let Some(dir) = std::env::var_os("AD_WAV_DIR") else {
        return Ok(());
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    let played = host.played_sounds();
    for (i, event) in played.iter().enumerate() {
        let (name, s) = (&event.name, &event.sound);
        let safe: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let path = dir.join(format!("{i:03}_{safe}.wav"));
        let mut w = Vec::with_capacity(44 + s.samples.len());
        let data_len = s.samples.len() as u32;
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&1u16.to_le_bytes()); // mono
        w.extend_from_slice(&s.rate_hz.to_le_bytes());
        w.extend_from_slice(&s.rate_hz.to_le_bytes()); // byte rate (8-bit mono)
        w.extend_from_slice(&1u16.to_le_bytes()); // block align
        w.extend_from_slice(&8u16.to_le_bytes()); // bits
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        w.extend_from_slice(&s.samples);
        std::fs::write(&path, w)?;
    }
    if !played.is_empty() {
        println!(
            "sounds      {} played, WAVs in {}",
            played.len(),
            dir.display()
        );
    }
    Ok(())
}

/// `AD_MIX_WAV=<file>`: render **what the session would sound like**.
///
/// The per-sound WAVs prove the `snd ` decoder is right. They say nothing about
/// the path from there to a speaker — the timing, the channel model, whether one
/// effect cuts another off. This runs the real [`ad_runtime::Mixer`] over the real
/// event stream at the real tick offsets and writes the result, so the audio path
/// has an artefact that can be listened to and diffed, exactly as the PNG does
/// for the picture. The only thing it does not exercise is the driver.
fn render_mix(host: &Host) -> std::io::Result<()> {
    let Some(out) = std::env::var_os("AD_MIX_WAV") else {
        return Ok(());
    };
    const RATE: u32 = 22_254; // the Mac's own DAC rate; no resampling to argue about
    let events = host.sound_log();
    if events.is_empty() {
        return Ok(());
    }
    let mut mixer = ad_runtime::Mixer::new(RATE);
    // One tick is 1/60 s. Events carry the tick they happened at, so the render
    // reproduces the session's timing rather than butting effects together.
    let last_tick = events
        .iter()
        .map(|e| match e {
            ad_toolbox::snd::SoundEvent::Play(p) => p.at_tick,
            ad_toolbox::snd::SoundEvent::Stop { at_tick, .. } => *at_tick,
        })
        .max()
        .unwrap_or(0);
    let per_tick = (RATE / 60) as usize;
    // Two extra seconds so a sound that starts on the last tick is not cut off.
    let total = (last_tick as usize + 1) * per_tick + RATE as usize * 2;
    // 16-bit, unlike the per-sound WAVs. The mixer works in f32 and these
    // effects are quiet — "Flap 1" peaks at 25 of 128 — so quantising the mix
    // back to 8 bits would throw away most of what there is to hear.
    let mut samples: Vec<i16> = Vec::with_capacity(total);
    let mut next = 0usize;
    let mut buf = vec![0.0f32; per_tick];
    for tick in 0..=(last_tick as usize + 120) {
        while let Some(event) = events.get(next) {
            let at = match event {
                ad_toolbox::snd::SoundEvent::Play(p) => p.at_tick,
                ad_toolbox::snd::SoundEvent::Stop { at_tick, .. } => *at_tick,
            };
            if at as usize > tick {
                break;
            }
            match event {
                ad_toolbox::snd::SoundEvent::Play(p) => mixer.play(p),
                ad_toolbox::snd::SoundEvent::Stop { channel, .. } => mixer.stop(*channel),
            }
            next += 1;
        }
        mixer.fill(&mut buf);
        samples.extend(buf.iter().map(|&s| (s * 32_767.0) as i16));
    }
    let path = PathBuf::from(out);
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + samples.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in &samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&path, w)?;
    println!(
        "mix         {} events over {} ticks -> {}",
        events.len(),
        last_tick,
        path.display()
    );
    Ok(())
}

fn report(host: &Host, frames: u32) {
    let fb = host.framebuffer();
    println!();
    println!("frames      {frames}");
    // `ink` is the honest signal: pixels differing from the dominant colour. A
    // screen blanked to black is 100% "non-zero" while showing nothing.
    println!(
        "ink         {} / {}   colours={}",
        fb.ink(),
        fb.pixels.len(),
        fb.distinct()
    );
    println!("traps       {} distinct", host.tb.log.summary_len());
    println!("trap history (first 24, PC relative to code base):");
    for c in host.tb.log.history.iter().take(24) {
        println!(
            "            ${:04X} {:<18} @ +{:#06x}",
            c.word,
            c.name.unwrap_or("?"),
            c.pc.wrapping_sub(ad_host_v2::CODE_BASE)
        );
    }
    println!("traps       {} distinct", host.tb.log.distinct());
    println!("            {}", host.tb.log.summary());
    if !host.tb.log.unimplemented.is_empty() {
        println!("UNIMPLEMENTED:");
        for (w, n) in &host.tb.log.unimplemented {
            let name = ad_toolbox::traps::name_of(*w).unwrap_or("?");
            println!("            ${w:04X} _{name} x{n}");
        }
    }
    println!("heap used   {} bytes", host.tb.mem.heap_used());
    println!("handles     {} live", host.tb.mem.live_handles());
    if !host.tb.mem.faults.is_empty() {
        println!("faults      {}", host.tb.mem.faults.len());
        for f in host.tb.mem.faults.iter().take(8) {
            println!(
                "            {:#010x} {} {}",
                f.addr,
                if f.write { "write" } else { "read " },
                f.note
            );
        }
    }
}

/// Minimal PNG writer, so the runtime keeps no image dependencies.
// The PNG writer used to live here, and a copy of it lived in the launcher.
// Both now call `ad_runtime::png`, because writing a picture is part of this
// project's *evidence* path and two implementations of it is one too many.
fn write_png(
    path: &std::path::Path,
    width: u16,
    height: u16,
    rgb: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    ad_runtime::png::write_rgb(path, u32::from(width), u32::from(height), rgb)?;
    Ok(())
}
