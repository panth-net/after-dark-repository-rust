//! The speaker: a real audio output stream fed by [`crate::Mixer`].
//!
//! Behind the `audio` feature, because the compatibility lab has no business
//! opening an audio device — 66 modules run headless in CI, and a build that
//! links a sound backend there is a build that can fail for reasons unrelated to
//! any module.
//!
//! # Why the mixer is behind a lock
//!
//! The output callback runs on the backend's own high-priority thread while the
//! emulator runs on ours, and the emulator is the thing that knows a sound
//! started. A `Mutex` is the right tool at this size: the critical section is a
//! few hundred adds and the buffer is milliseconds long, so contention is not a
//! real risk, and getting it wrong the clever way costs correctness for nothing.
//! The callback never allocates and never blocks on anything but this lock.

use std::sync::{Arc, Mutex};

use ad_toolbox::snd::SoundEvent;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

use crate::Mixer;

/// A live output stream. Dropping it stops the sound.
pub struct AudioDevice {
    mixer: Arc<Mutex<Mixer>>,
    /// Held to keep the stream alive; cpal stops on drop.
    _stream: cpal::Stream,
    rate: u32,
}

impl std::fmt::Debug for AudioDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioDevice")
            .field("rate", &self.rate)
            .finish()
    }
}

impl AudioDevice {
    /// Open the default output device.
    ///
    /// # Errors
    /// A message describing why there is no usable output. Callers should carry
    /// on **silently** rather than refuse to run: a screen saver that will not
    /// start because a machine has no sound card is worse than a quiet one.
    pub fn open() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no default audio output device")?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("default output config: {e}"))?;
        let rate = config.sample_rate().0;
        let channels = usize::from(config.channels()).max(1);
        let mixer = Arc::new(Mutex::new(Mixer::new(rate)));

        // One mono buffer, reused, fanned out to however many channels the
        // device wants. These are mono 8-bit effects from 1991; there is no
        // stereo information to preserve and inventing a pan would be a fiction.
        let mono = Arc::new(Mutex::new(Vec::<f32>::new()));
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let mixer = Arc::clone(&mixer);
                let mono = Arc::clone(&mono);
                device.build_output_stream(
                    &config.into(),
                    move |out: &mut [f32], _| {
                        let frames = out.len() / channels;
                        let Ok(mut buf) = mono.lock() else { return };
                        buf.clear();
                        buf.resize(frames, 0.0);
                        if let Ok(mut m) = mixer.lock() {
                            m.fill(&mut buf);
                        }
                        for (f, chunk) in out.chunks_mut(channels).enumerate() {
                            let v = buf.get(f).copied().unwrap_or(0.0);
                            for slot in chunk.iter_mut() {
                                *slot = v;
                            }
                        }
                    },
                    |e| eprintln!("audio stream error: {e}"),
                    None,
                )
            }
            other => {
                // Every platform cpal supports offers f32; a device that does
                // not is reported rather than guessed at.
                return Err(format!("unsupported sample format {other:?}"));
            }
        }
        .map_err(|e| format!("build output stream: {e}"))?;
        stream.play().map_err(|e| format!("start stream: {e}"))?;
        Ok(Self {
            mixer,
            _stream: stream,
            rate,
        })
    }

    /// The device's sample rate.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Apply everything the module did to the sound hardware since last time.
    ///
    /// Called from the host's present hook, because a game never returns from
    /// `DrawFrame` — the same reason the window is driven from there.
    pub fn submit(&self, events: &[SoundEvent]) {
        if events.is_empty() {
            return;
        }
        let Ok(mut m) = self.mixer.lock() else { return };
        for event in events {
            match event {
                SoundEvent::Play(p) => m.play(p),
                SoundEvent::Stop { channel, .. } => m.stop(*channel),
            }
        }
    }

    /// Silence everything — module close, or the user quitting.
    pub fn silence(&self) {
        if let Ok(mut m) = self.mixer.lock() {
            m.stop_all();
        }
    }
}
