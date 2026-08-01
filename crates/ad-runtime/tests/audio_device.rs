//! The one thing the mixer's unit tests cannot cover: that a real output device
//! opens, accepts events, and stops.
//!
//! Written to **skip** when there is no usable device rather than to fail. CI
//! runs headless with no sound card, and a test that failed there would be
//! reporting a fact about the runner rather than about this code. What it must
//! never do is silently pass while the device path is broken, so the skip prints
//! its reason and the assertions are real when a device exists.

#![cfg(feature = "audio")]

use std::sync::Arc;

use ad_runtime::AudioDevice;
use ad_toolbox::snd::{DecodedSound, PlayEvent, SoundEvent};

#[test]
fn a_real_device_opens_and_takes_events() {
    let device = match AudioDevice::open() {
        Ok(d) => d,
        Err(e) => {
            println!("skipped: no audio output on this machine ({e})");
            return;
        }
    };
    assert!(device.rate() >= 8_000, "implausible rate {}", device.rate());

    // A short square wave at the Mac's own DAC rate.
    let sound = Arc::new(DecodedSound {
        samples: (0..2_000u32)
            .map(|i| if (i / 20) % 2 == 0 { 200 } else { 56 })
            .collect(),
        rate_hz: 22_254,
        loop_range: None,
    });
    device.submit(&[
        SoundEvent::Play(PlayEvent {
            name: "test tone".into(),
            channel: 1,
            at_tick: 0,
            sound,
        }),
        SoundEvent::Stop {
            channel: 1,
            at_tick: 1,
        },
    ]);
    // Submitting an empty slice must be a no-op, not a lock or a panic: the
    // present hook calls this every tick and almost always has nothing.
    device.submit(&[]);
    device.silence();
}
