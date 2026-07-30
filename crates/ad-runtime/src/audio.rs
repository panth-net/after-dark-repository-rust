//! Turning played sounds into samples a speaker can take.
//!
//! # The channel model, and what is not yet known
//!
//! The Sound Manager plays **one sound at a time per `SndChannel`**: a new
//! `SndPlay` on a busy channel either replaces what is playing or queues behind
//! it, depending on flags the After Dark sound extension sets inside glue this
//! project cannot see. [`Mixer`] models replace-on-play, because that is the
//! behaviour that cannot produce an artefact the original could not: queueing
//! would let a game firing fifty-nine shots fall audibly seconds behind itself,
//! which no shipped screen saver did.
//!
//! Different channels mix. Modules on this disk open one channel, so in practice
//! one effect plays at a time — which is why the uncertainty above is recorded
//! rather than papered over. The evidence that would settle it is the ROM oracle:
//! boot the original under QEMU and record what it actually sounds like.
//!
//! # Looping, and why there is none
//!
//! `SoundHeader` carries `loopStart`/`loopEnd`, and a first version of this
//! mixer honoured them. It was wrong twice over.
//!
//! Inside Macintosh is explicit: with `bufferCmd` the sound plays **once**, and
//! the loop points are used only when a sample is played as a *note* with a
//! frequency and duration command. Every play that reaches this runtime comes
//! from `SndPlay` or the After Dark extension's `PlaySound`, both of which are
//! buffer plays.
//!
//! Flying Toasters confirms it from the other direction. Its three effects store
//!
//! ```text
//! 'Flap 1'  len=1568  loopStart=1566  loopEnd=1567
//! 'Flap 2'  len=1312  loopStart=1310  loopEnd=1311
//! 'Mix 1'   len=1888  loopStart=1886  loopEnd=1887
//! ```
//!
//! — a one-sample window on the last sample, which is the idiom for "this is a
//! one-shot": the fields cannot be zero for note playback, so they are parked at
//! the end. Honouring them held that single sample forever, so a five-second
//! render of a screen saver came out as a continuous DC hum at 27% of full scale
//! instead of discrete flaps. `DecodedSound::loop_range` still records what the
//! resource says, because that is what the resource says; the mixer does not act
//! on it.
//!
//! # Resampling
//!
//! Nearest-neighbour, deliberately. These are 8-bit effects recorded at 22.254
//! kHz for a DAC that did no interpolation, so nearest is closer to what the
//! hardware produced than a smooth resampler would be. It is also exact whenever
//! the output rate is a multiple of the source rate, which is the common case.

use std::collections::BTreeMap;
use std::sync::Arc;

use ad_toolbox::snd::{DecodedSound, PlayEvent};

/// One playing sound on one channel.
#[derive(Debug)]
struct Voice {
    sound: Arc<DecodedSound>,
    /// Position in **output** samples, so the source index is derived and no
    /// rounding error accumulates.
    out_pos: u64,
}

/// Mixes the sounds a module played into an output buffer.
#[derive(Debug)]
pub struct Mixer {
    voices: BTreeMap<u32, Voice>,
    out_rate: u32,
}

impl Mixer {
    /// A mixer producing `out_rate` samples per second.
    #[must_use]
    pub fn new(out_rate: u32) -> Self {
        Self {
            voices: BTreeMap::new(),
            out_rate: out_rate.max(1),
        }
    }

    /// Start a sound, replacing whatever that channel was playing.
    pub fn play(&mut self, event: &PlayEvent) {
        if event.sound.samples.is_empty() {
            return;
        }
        self.voices.insert(
            event.channel,
            Voice {
                sound: Arc::clone(&event.sound),
                out_pos: 0,
            },
        );
    }

    /// Stop one channel — `QuietSound`, `FlushSound`, `SndDisposeChannel`.
    pub fn stop(&mut self, channel: u32) {
        self.voices.remove(&channel);
    }

    /// Stop everything.
    pub fn stop_all(&mut self) {
        self.voices.clear();
    }

    /// How many channels are sounding.
    #[must_use]
    pub fn active(&self) -> usize {
        self.voices.len()
    }

    /// Fill `out` with the next mono frames, in `-1.0..=1.0`.
    ///
    /// Overwrites rather than adds, so a device callback cannot accidentally
    /// leave the previous buffer's contents behind as a repeating click.
    pub fn fill(&mut self, out: &mut [f32]) {
        for slot in out.iter_mut() {
            *slot = 0.0;
        }
        let mut finished: Vec<u32> = Vec::new();
        for (&channel, voice) in &mut self.voices {
            let src_rate = u64::from(voice.sound.rate_hz.max(1));
            let out_rate = u64::from(self.out_rate);
            let mut done = false;
            for slot in out.iter_mut() {
                // Nearest source sample for this output position. No loop: see
                // the module header — `bufferCmd` plays once, and this disk's
                // loop points are the one-shot idiom.
                let idx = voice.out_pos.saturating_mul(src_rate) / out_rate;
                let Some(&sample) = voice.sound.samples.get(idx as usize) else {
                    done = true;
                    break;
                };
                // 8-bit unsigned, 0x80 silence, at **unity gain**. Attenuating
                // would be unfaithful and, for this disk, inaudible: Flying
                // Toasters' "Flap 1" peaks at only 25 of a possible 128, so these
                // effects were recorded quiet and any headroom taken here is
                // headroom the original did not take. Modules open one channel, so
                // in practice one voice plays at full scale; the clamp below is
                // there for the case that cannot happen on this disk rather than
                // as a mixing policy.
                *slot += (f32::from(sample) - 128.0) / 128.0;
                voice.out_pos = voice.out_pos.saturating_add(1);
            }
            if done {
                finished.push(channel);
            }
        }
        for channel in finished {
            self.voices.remove(&channel);
        }
        for slot in out.iter_mut() {
            *slot = slot.clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(channel: u32, samples: Vec<u8>, rate_hz: u32) -> PlayEvent {
        PlayEvent {
            name: "test".into(),
            channel,
            at_tick: 0,
            sound: Arc::new(DecodedSound {
                samples,
                rate_hz,
                loop_range: None,
            }),
        }
    }

    #[test]
    fn silence_when_nothing_is_playing() {
        let mut m = Mixer::new(44_100);
        let mut buf = [1.0f32; 16];
        m.fill(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0), "{buf:?}");
    }

    #[test]
    fn doubling_the_rate_holds_each_sample_twice() {
        // The exact case: 22050 -> 44100. Nearest-neighbour must not drift.
        let mut m = Mixer::new(44_100);
        m.play(&event(1, vec![128, 255, 128, 0], 22_050));
        let mut buf = [0.0f32; 8];
        m.fill(&mut buf);
        assert_eq!(buf[0], buf[1]);
        assert_eq!(buf[2], buf[3]);
        assert_eq!(buf[4], buf[5]);
        assert_eq!(buf[6], buf[7]);
        assert!(buf[2] > 0.0 && buf[6] < 0.0, "{buf:?}");
    }

    #[test]
    fn a_finished_sound_stops_and_leaves_silence() {
        let mut m = Mixer::new(22_050);
        m.play(&event(1, vec![255; 4], 22_050));
        let mut buf = [0.0f32; 4];
        m.fill(&mut buf);
        assert_eq!(m.active(), 1, "still exactly at the end, not past it");
        m.fill(&mut buf);
        assert_eq!(m.active(), 0);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_new_play_replaces_the_channels_current_sound() {
        let mut m = Mixer::new(22_050);
        m.play(&event(1, vec![255; 1000], 22_050));
        m.play(&event(1, vec![128; 1000], 22_050));
        assert_eq!(m.active(), 1, "one channel, one voice");
        let mut buf = [0.0f32; 4];
        m.fill(&mut buf);
        // 128 is silence: the *second* sound is what plays.
        assert!(buf.iter().all(|&s| s == 0.0), "{buf:?}");
    }

    #[test]
    fn separate_channels_mix() {
        // Quiet sources, so the sum has room and the test measures mixing rather
        // than the clamp. Real effects on this disk peak at a fifth of full scale.
        let mut m = Mixer::new(22_050);
        m.play(&event(1, vec![150; 8], 22_050));
        m.play(&event(2, vec![150; 8], 22_050));
        assert_eq!(m.active(), 2);
        let mut two = [0.0f32; 4];
        m.fill(&mut two);

        let mut m1 = Mixer::new(22_050);
        m1.play(&event(1, vec![150; 8], 22_050));
        let mut one = [0.0f32; 4];
        m1.fill(&mut one);
        assert!(two[0] > one[0], "two channels must be louder: {two:?} {one:?}");
    }

    #[test]
    fn a_single_voice_reaches_full_scale() {
        // The original played straight into the DAC. A mixer that quietly
        // attenuated would make these already-quiet effects inaudible.
        let mut m = Mixer::new(22_050);
        m.play(&event(1, vec![255, 0], 22_050));
        let mut buf = [0.0f32; 2];
        m.fill(&mut buf);
        assert!((buf[0] - 0.9922).abs() < 0.001, "{buf:?}");
        assert!((buf[1] + 1.0).abs() < 0.001, "{buf:?}");
    }

    #[test]
    fn output_never_leaves_the_valid_range() {
        let mut m = Mixer::new(22_050);
        for ch in 0..32 {
            m.play(&event(ch, vec![255; 64], 22_050));
        }
        let mut buf = [0.0f32; 16];
        m.fill(&mut buf);
        assert!(buf.iter().all(|&s| (-1.0..=1.0).contains(&s)), "{buf:?}");
    }

    #[test]
    fn the_one_shot_loop_idiom_does_not_hum_forever() {
        // Flying Toasters' real shape: loop points parked on the last sample,
        // which is how a one-shot is stored. Honouring them held that sample
        // forever and turned a screen saver into a continuous DC tone.
        let mut m = Mixer::new(22_050);
        let mut e = event(1, vec![255; 8], 22_050);
        e.sound = Arc::new(DecodedSound {
            samples: vec![255; 8],
            rate_hz: 22_050,
            loop_range: Some((6, 7)),
        });
        m.play(&e);
        let mut buf = [0.0f32; 8];
        m.fill(&mut buf);
        assert!(buf.iter().all(|&s| s > 0.0), "the sound itself must play");
        m.fill(&mut buf);
        assert_eq!(m.active(), 0, "and then end");
        assert!(buf.iter().all(|&s| s == 0.0), "leaving silence: {buf:?}");
    }

    #[test]
    fn an_empty_sound_is_not_a_voice() {
        let mut m = Mixer::new(44_100);
        m.play(&event(1, vec![], 22_050));
        assert_eq!(m.active(), 0);
    }
}
