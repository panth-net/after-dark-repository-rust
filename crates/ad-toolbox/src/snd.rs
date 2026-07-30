//! `snd ` — the Sound Manager's resource format, decoded to PCM.
//!
//! Twenty-four modules on the disk carry sampled sound; Lunatic Fringe alone
//! has twenty-one effects and calls the After Dark sound extension for every
//! shot and explosion. The extension and trap layers were stack-correct but
//! silent; this module turns the resource bytes into samples a host can play.
//!
//! # Format, as the resources on this disk actually use it
//!
//! A format-1 resource is: format word, a synth list (`count`, then
//! `{synthID, initOption}` pairs), a command list (`count`, then 8-byte
//! `SndCommand`s), and data. The command that matters is `bufferCmd`/`soundCmd`
//! with the "data offset" bit set: its second parameter is the offset of a
//! `SoundHeader` within the resource. A format-2 resource (HyperCard's) skips
//! the synth list. The standard header is 22 bytes — pointer, length, Fixed
//! sample rate, loop bounds, encode byte, base frequency — followed by 8-bit
//! unsigned samples. The extended header (encode `0xFF`) adds channel count
//! and bit depth for the same idea.

/// One decoded sound: mono PCM, 8-bit unsigned samples as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSound {
    /// Samples, 8-bit unsigned (0x80 is silence).
    pub samples: Vec<u8>,
    /// Sample rate in Hz, from the header's Fixed value.
    pub rate_hz: u32,
    /// Loop bounds in samples, when the header declares a loop.
    pub loop_range: Option<(u32, u32)>,
}

impl DecodedSound {
    /// Length in 60ths of a second, the unit `GetSoundLength` answers in.
    #[must_use]
    pub fn ticks(&self) -> u32 {
        if self.rate_hz == 0 {
            return 0;
        }
        (self.samples.len() as u32).saturating_mul(60) / self.rate_hz
    }
}

/// One sound the module asked to play.
#[derive(Debug, Clone)]
pub struct PlayEvent {
    /// The `snd ` resource's own name ("Normal Shot", "Player Death") when the
    /// handle is a live resource, else a hex handle.
    pub name: String,
    /// The `SndChannel` it was played on. The Sound Manager plays one sound at a
    /// time per channel, so this is what an output device keys a voice on.
    pub channel: u32,
    /// Tick the play was requested at, so a host can pace or drop late audio.
    pub at_tick: u32,
    /// The decoded PCM, shared: playing the same effect fifty-nine times decodes
    /// it once.
    pub sound: std::sync::Arc<DecodedSound>,
}

/// One thing that happened to the sound hardware, in order.
///
/// Plays and stops are one stream because their *order* is what an output device
/// needs: a game that fires, stops the channel, and fires again must not have the
/// stop applied after the second shot started.
#[derive(Debug, Clone)]
pub enum SoundEvent {
    Play(PlayEvent),
    /// `QuietSound`, `FlushSound` or `SndDisposeChannel`: silence one channel.
    Stop { channel: u32, at_tick: u32 },
}

/// Everything a module has played, decoded once and cached.
///
/// This replaces a `Vec<(String, DecodedSound)>` that re-decoded the resource on
/// **every** play and never drained — Lunatic Fringe fired fifty-nine shots in
/// one session and decoded the same twenty-kilobyte resource fifty-nine times,
/// keeping all fifty-nine copies. The cache is keyed on the resource bytes, so it
/// is correct for handles that are not resources at all.
#[derive(Debug, Default)]
pub struct SoundBank {
    cache: std::collections::BTreeMap<u64, std::sync::Arc<DecodedSound>>,
    log: Vec<SoundEvent>,
    /// How much of `log` an output device has already been given.
    drained: usize,
    /// Events dropped from the front of `log` to bound it. See [`LOG_CAP`].
    dropped: u64,
}

/// Most events [`SoundBank`] keeps.
///
/// The log used to grow for the lifetime of the process. That is harmless in the
/// lab, where a run is twenty frames, and an unbounded leak in the product, where
/// somebody plays Lunatic Fringe for an hour and every shot and explosion is
/// recorded forever with its own `String` name.
///
/// Only events an output device has already been handed are dropped, so bounding
/// the log can never swallow a sound before it is heard. Far above any 20-frame
/// run, so the compatibility survey is unaffected.
const LOG_CAP: usize = 4096;

impl SoundBank {
    /// Decode `bytes` if they are new, and record a play.
    ///
    /// Returns the decoded sound so a caller can log it, or `None` if the bytes
    /// are not a playable sampled sound — a module handing the sound extension
    /// something else is not an error the module can act on.
    pub fn play(
        &mut self,
        name: String,
        channel: u32,
        at_tick: u32,
        bytes: &[u8],
    ) -> Option<std::sync::Arc<DecodedSound>> {
        let key = fnv1a(bytes);
        let sound = match self.cache.get(&key) {
            Some(s) => std::sync::Arc::clone(s),
            None => {
                let decoded = std::sync::Arc::new(decode(bytes).ok()?);
                self.cache.insert(key, std::sync::Arc::clone(&decoded));
                decoded
            }
        };
        self.log.push(SoundEvent::Play(PlayEvent {
            name,
            channel,
            at_tick,
            sound: std::sync::Arc::clone(&sound),
        }));
        Some(sound)
    }

    /// Record that a channel was silenced.
    pub fn stop(&mut self, channel: u32, at_tick: u32) {
        self.log.push(SoundEvent::Stop { channel, at_tick });
    }

    /// Every event, in order.
    #[must_use]
    pub fn log(&self) -> &[SoundEvent] {
        &self.log
    }

    /// Just the plays — the lab's evidence, and what the WAV dump writes.
    pub fn plays(&self) -> impl Iterator<Item = &PlayEvent> {
        self.log.iter().filter_map(|e| match e {
            SoundEvent::Play(p) => Some(p),
            SoundEvent::Stop { .. } => None,
        })
    }

    /// Distinct sounds decoded. The gap between this and `log().len()` is what
    /// the cache saved.
    #[must_use]
    pub fn decoded_count(&self) -> usize {
        self.cache.len()
    }

    /// Plays not yet handed to an output device.
    ///
    /// Separate from `log` because the lab needs the whole history and a speaker
    /// needs only what is new. Draining does not free anything: an effect played
    /// once is very likely to be played again.
    pub fn drain_new(&mut self) -> Vec<SoundEvent> {
        let fresh = self.log.get(self.drained..).unwrap_or_default().to_vec();
        self.drained = self.log.len();
        self.trim();
        fresh
    }

    /// Drop delivered events once the log is over [`LOG_CAP`].
    ///
    /// Called from `drain_new` rather than from `play`, because that is the point
    /// at which events become droppable: a host with no output device never
    /// drains, and its log is bounded by the run instead.
    fn trim(&mut self) {
        let over = self.log.len().saturating_sub(LOG_CAP);
        // Never drop an event a device has not been given yet.
        let n = over.min(self.drained);
        if n == 0 {
            return;
        }
        self.log.drain(..n);
        self.drained = self.drained.saturating_sub(n);
        self.dropped = self.dropped.saturating_add(n as u64);
    }

    /// Events dropped to keep the log bounded, so a total is never quietly wrong.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// FNV-1a, for keying the decode cache on the resource bytes.
///
/// Not a security boundary: a collision would play the wrong effect, which is
/// why it is over the *whole* payload rather than a prefix.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Why a resource would not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SndError {
    Truncated,
    /// Format word neither 1 nor 2.
    UnknownFormat(u16),
    /// No buffer/sound command carrying sample data.
    NoSampleCommand,
    /// A compressed (`0xFE`) or otherwise unknown encode byte.
    UnsupportedEncoding(u8),
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]))
}
fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(at)?,
        *d.get(at + 1)?,
        *d.get(at + 2)?,
        *d.get(at + 3)?,
    ]))
}

/// Decode a `snd ` resource's bytes.
///
/// # Errors
/// [`SndError`] when the bytes are not a playable sampled sound.
pub fn decode(data: &[u8]) -> Result<DecodedSound, SndError> {
    let format = be16(data, 0).ok_or(SndError::Truncated)?;
    let mut at = 2usize;
    match format {
        1 => {
            let synths = usize::from(be16(data, at).ok_or(SndError::Truncated)?);
            // Each synth entry is a word ID and a long init option.
            at = at + 2 + synths * 6;
        }
        2 => {
            at += 2; // reference count, unused
        }
        other => return Err(SndError::UnknownFormat(other)),
    }
    let n_cmds = usize::from(be16(data, at).ok_or(SndError::Truncated)?);
    at += 2;
    for _ in 0..n_cmds {
        let cmd = be16(data, at).ok_or(SndError::Truncated)?;
        let _param1 = be16(data, at + 2).ok_or(SndError::Truncated)?;
        let param2 = be32(data, at + 4).ok_or(SndError::Truncated)?;
        at += 8;
        // bufferCmd (81) or soundCmd (80), with bit 15 = "param2 is an offset
        // into this resource" — which is the only form a resource can hold.
        if cmd & 0x7FFF == 80 || cmd & 0x7FFF == 81 {
            return decode_header(data, param2 as usize);
        }
    }
    Err(SndError::NoSampleCommand)
}

/// Decode a `SoundHeader` (standard or extended) at `at`.
fn decode_header(data: &[u8], at: usize) -> Result<DecodedSound, SndError> {
    let length = be32(data, at + 4).ok_or(SndError::Truncated)? as usize;
    let rate_fixed = be32(data, at + 8).ok_or(SndError::Truncated)?;
    let loop_start = be32(data, at + 12).ok_or(SndError::Truncated)?;
    let loop_end = be32(data, at + 16).ok_or(SndError::Truncated)?;
    let encode = *data.get(at + 20).ok_or(SndError::Truncated)?;
    let rate_hz = rate_fixed >> 16; // Fixed 16.16 -> whole Hz is plenty
    match encode {
        // Standard header: `length` samples of 8-bit mono right behind it.
        0x00 => {
            let start = at + 22;
            let end = start.saturating_add(length).min(data.len());
            let samples = data.get(start..end).ok_or(SndError::Truncated)?.to_vec();
            Ok(DecodedSound {
                samples,
                rate_hz,
                loop_range: (loop_end > loop_start).then_some((loop_start, loop_end)),
            })
        }
        // Extended header: `length` here is FRAMES; channels at +20..? No —
        // numChannels replaces length's meaning: length = channels, and the
        // frame count is the long at +22. 64 bytes total before samples.
        0xFF => {
            let channels = length.max(1);
            let frames = be32(data, at + 22).ok_or(SndError::Truncated)? as usize;
            let bits = be16(data, at + 48).map_or(8, usize::from);
            let start = at + 64;
            let bytes_per_sample = (bits / 8).max(1);
            let want = frames * channels * bytes_per_sample;
            let end = start.saturating_add(want).min(data.len());
            let raw = data.get(start..end).ok_or(SndError::Truncated)?;
            // Fold to 8-bit unsigned mono: take channel 0, high byte of each
            // sample, flipping 16-bit signed to unsigned.
            let mut samples = Vec::with_capacity(frames);
            for f in 0..frames {
                let idx = f * channels * bytes_per_sample;
                let Some(&b) = raw.get(idx) else { break };
                samples.push(if bytes_per_sample >= 2 { b ^ 0x80 } else { b });
            }
            Ok(DecodedSound {
                samples,
                rate_hz,
                loop_range: None,
            })
        }
        other => Err(SndError::UnsupportedEncoding(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a format-1 resource around a standard header.
    fn snd1(samples: &[u8], rate_hz: u32) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&1u16.to_be_bytes()); // format
        d.extend_from_slice(&1u16.to_be_bytes()); // one synth
        d.extend_from_slice(&5u16.to_be_bytes()); // sampledSynth
        d.extend_from_slice(&0xA0u32.to_be_bytes()); // init option
        d.extend_from_slice(&1u16.to_be_bytes()); // one command
        d.extend_from_slice(&0x8051u16.to_be_bytes()); // bufferCmd, offset form
        d.extend_from_slice(&0u16.to_be_bytes()); // param1
        d.extend_from_slice(&20u32.to_be_bytes()); // param2: header at 20
        assert_eq!(d.len(), 20);
        d.extend_from_slice(&0u32.to_be_bytes()); // samplePtr
        d.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        d.extend_from_slice(&(rate_hz << 16).to_be_bytes());
        d.extend_from_slice(&0u32.to_be_bytes()); // loopStart
        d.extend_from_slice(&0u32.to_be_bytes()); // loopEnd
        d.push(0); // encode: standard
        d.push(60); // baseFrequency: middle C
        d.extend_from_slice(samples);
        d
    }

    #[test]
    fn decodes_a_standard_format_one_resource() {
        let d = snd1(&[0x80, 0x90, 0x70, 0x80], 22254);
        let s = decode(&d).expect("decode");
        assert_eq!(s.samples, vec![0x80, 0x90, 0x70, 0x80]);
        assert_eq!(s.rate_hz, 22254);
        assert_eq!(s.loop_range, None);
    }

    #[test]
    fn length_in_ticks_follows_the_sample_rate() {
        // 11127 samples at 22254 Hz is half a second: 30 ticks.
        let d = snd1(&vec![0x80; 11127], 22254);
        let s = decode(&d).expect("decode");
        assert_eq!(s.ticks(), 30);
    }

    #[test]
    fn refuses_compressed_sounds_rather_than_playing_noise() {
        let mut d = snd1(&[0x80; 4], 22254);
        d[20 + 20] = 0xFE; // compressed header
        assert_eq!(decode(&d), Err(SndError::UnsupportedEncoding(0xFE)));
    }

    #[test]
    fn refuses_unknown_formats() {
        let mut d = snd1(&[0x80; 4], 22254);
        d[1] = 3;
        assert_eq!(decode(&d), Err(SndError::UnknownFormat(3)));
    }

    #[test]
    fn the_same_effect_fired_fifty_nine_times_decodes_once() {
        // Lunatic Fringe's shot count in one measured session. The old code
        // decoded the resource on every play and kept every copy.
        let d = snd1(&[0x80; 4096], 22254);
        let mut bank = SoundBank::default();
        for i in 0..59 {
            bank.play("Normal Shot".into(), 1, i, &d).expect("decode");
        }
        assert_eq!(bank.plays().count(), 59, "every play is still recorded");
        assert_eq!(bank.decoded_count(), 1, "and decoded exactly once");
        // Every event shares one allocation.
        let first = std::sync::Arc::clone(&bank.plays().next().expect("a play").sound);
        assert!(bank
            .plays()
            .all(|e| std::sync::Arc::ptr_eq(&e.sound, &first)));
    }

    #[test]
    fn different_sounds_are_cached_separately() {
        let a = snd1(&[0x80; 8], 22254);
        let b = snd1(&[0x40; 8], 22254);
        let mut bank = SoundBank::default();
        bank.play("a".into(), 1, 0, &a).expect("a");
        bank.play("b".into(), 1, 0, &b).expect("b");
        assert_eq!(bank.decoded_count(), 2);
    }

    #[test]
    fn draining_yields_each_play_exactly_once() {
        let d = snd1(&[0x80; 8], 22254);
        let mut bank = SoundBank::default();
        bank.play("one".into(), 1, 0, &d);
        assert_eq!(bank.drain_new().len(), 1);
        assert!(bank.drain_new().is_empty(), "a drained play must not repeat");
        bank.play("two".into(), 1, 5, &d);
        bank.stop(1, 6);
        let fresh = bank.drain_new();
        assert_eq!(fresh.len(), 2, "the play and the stop, in order");
        assert!(matches!(&fresh[0], SoundEvent::Play(p) if p.name == "two"));
        assert!(matches!(fresh[1], SoundEvent::Stop { channel: 1, .. }));
        assert_eq!(bank.plays().count(), 2, "the full history survives draining");
    }

    #[test]
    fn a_handle_that_is_not_a_sound_is_ignored_not_fatal() {
        let mut bank = SoundBank::default();
        assert!(bank.play("junk".into(), 1, 0, b"not a sound").is_none());
        assert!(bank.log().is_empty());
    }
}

#[cfg(test)]
mod log_bound_tests {
    use super::*;

    /// A tiny valid `snd ` format-1 resource with one sampled-sound command.
    fn one_sound() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&1u16.to_be_bytes()); // format 1
        d.extend_from_slice(&1u16.to_be_bytes()); // one data type
        d.extend_from_slice(&5u16.to_be_bytes()); // sampledSynth
        d.extend_from_slice(&0u32.to_be_bytes()); // init
        d.extend_from_slice(&1u16.to_be_bytes()); // one command
        d.extend_from_slice(&0x8051u16.to_be_bytes()); // bufferCmd + data offset
        d.extend_from_slice(&0u16.to_be_bytes()); // param1
        d.extend_from_slice(&20u32.to_be_bytes()); // param2: offset of the header
        // Sound header at offset 20.
        d.extend_from_slice(&0u32.to_be_bytes()); // samplePtr = 0 (inline)
        d.extend_from_slice(&4u32.to_be_bytes()); // length
        d.extend_from_slice(&0x56EE_8BA3u32.to_be_bytes()); // 22 kHz fixed
        d.extend_from_slice(&0u32.to_be_bytes()); // loopStart
        d.extend_from_slice(&0u32.to_be_bytes()); // loopEnd
        d.push(0); // encode = standard
        d.push(60); // baseFrequency
        d.extend_from_slice(&[0x80, 0x90, 0x70, 0x88]); // samples
        d
    }

    /// The log is bounded, and only ever drops what a device already received.
    ///
    /// It used to grow for the life of the process: fine for a 20-frame survey,
    /// an unbounded leak for someone playing a game for an hour.
    #[test]
    fn the_log_is_bounded_but_never_loses_an_undelivered_sound() {
        let mut bank = SoundBank::default();
        let bytes = one_sound();

        // Without a device draining, nothing may be dropped — a host with no
        // speaker must not silently lose the history the lab reads.
        for i in 0..(LOG_CAP + 500) {
            bank.play("s".into(), 1, i as u32, &bytes);
        }
        assert_eq!(bank.dropped(), 0, "undrained events are not droppable");
        assert_eq!(bank.plays().count(), LOG_CAP + 500);

        // Once delivered, the excess goes.
        let handed = bank.drain_new();
        assert_eq!(handed.len(), LOG_CAP + 500, "every event reached the device");
        assert_eq!(bank.dropped(), 500);
        assert_eq!(bank.plays().count(), LOG_CAP);

        // And it stays bounded across further rounds.
        for i in 0..1000 {
            bank.play("s".into(), 1, i as u32, &bytes);
        }
        let handed = bank.drain_new();
        assert_eq!(handed.len(), 1000, "no new event was skipped");
        assert_eq!(bank.plays().count(), LOG_CAP);
        assert_eq!(bank.dropped(), 1500);
    }

    /// Decoding is cached by bytes, so a repeated effect costs nothing.
    #[test]
    fn the_same_sound_decodes_once() {
        let mut bank = SoundBank::default();
        let bytes = one_sound();
        let a = bank.play("s".into(), 1, 0, &bytes).expect("decodes");
        let b = bank.play("s".into(), 1, 1, &bytes).expect("decodes");
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "the cache must hand back the same decoded sound"
        );
    }
}
