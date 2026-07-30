//! `_Random` — bit-exact.
//!
//! This matters more than its size suggests. 56 of the 66 modules on the After
//! Dark 2.0x disk call `_Random`, and Lunatic Fringe's enemy spawning runs on it
//! (see `docs/LEARNINGS.md`). Because Apple *documented* the algorithm, this is one of the
//! few places where the runtime can be provably identical to the original rather
//! than merely plausible — so it is implemented from the specification and
//! tested against hand-computed values.
//!
//! The algorithm (Inside Macintosh, and Apple's published QuickDraw source) is a
//! Lehmer generator over the `RndSeed` low-memory global:
//!
//! ```text
//! temp  = (RndSeed * 16807) mod (2^31 - 1)     computed so it cannot overflow
//! RndSeed = temp
//! result = low 16 bits of temp, as a signed 16-bit value
//! ```
//!
//! Apple's implementation performs the modulo with the classic Schrage-style
//! split to stay inside 32-bit arithmetic, and folds the sign the same way. The
//! observable behaviour is the recurrence below.

/// Lehmer multiplier.
const A: i64 = 16_807;
/// Mersenne prime modulus, 2^31 - 1.
const M: i64 = 2_147_483_647;

/// Advance the seed and return the 16-bit result, exactly as `_Random` does.
///
/// Returns `(new_seed, result)`. The result is the low word of the new seed
/// interpreted as a signed 16-bit integer, which is why `_Random` can return
/// negative numbers — a detail modules rely on when they mask or take absolute
/// values.
#[must_use]
pub fn next(seed: u32) -> (u32, i16) {
    // Seed 0 is a fixed point of the recurrence; Apple's code treats it as 1 so
    // the generator cannot get stuck.
    let s = if seed == 0 { 1 } else { i64::from(seed) };
    let mut t = (s.wrapping_mul(A)) % M;
    // The reference implementation keeps the residue non-negative.
    if t < 0 {
        t = t.wrapping_add(M);
    }
    let new_seed = t as u32;
    let result = (t as u32 & 0xFFFF) as i16;
    (new_seed, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_hand_computed_sequence_from_seed_1() {
        // 1 * 16807 = 16807; 16807 * 16807 = 282,475,249; and so on — all well
        // inside the modulus, so these are exact by inspection.
        let (s1, r1) = next(1);
        assert_eq!(s1, 16_807);
        assert_eq!(r1, 16_807);

        let (s2, r2) = next(s1);
        assert_eq!(s2, 282_475_249);
        assert_eq!(r2, (282_475_249u32 & 0xFFFF) as i16);

        let (s3, _) = next(s2);
        // 282,475,249 * 16,807 mod (2^31-1)
        assert_eq!(s3, ((282_475_249i64 * 16_807) % M) as u32);
    }

    #[test]
    fn result_is_the_signed_low_word() {
        // Find a step whose low word has the high bit set, and confirm the
        // result is negative. Modules that mask or negate depend on this.
        let mut seed = 1u32;
        let mut saw_negative = false;
        for _ in 0..200 {
            let (s, r) = next(seed);
            seed = s;
            assert_eq!(r, (s & 0xFFFF) as i16, "result must be the low word");
            if r < 0 {
                saw_negative = true;
            }
        }
        assert!(
            saw_negative,
            "_Random returns signed values; a test that never sees one proves nothing"
        );
    }

    #[test]
    fn seed_zero_does_not_stick() {
        let (s, _) = next(0);
        assert_ne!(s, 0, "0 is a fixed point of the recurrence and must be avoided");
        assert_eq!(s, 16_807, "treated as seed 1");
    }

    #[test]
    fn sequence_is_deterministic_and_reproducible() {
        let run = |mut seed: u32| {
            let mut out = Vec::new();
            for _ in 0..64 {
                let (s, r) = next(seed);
                seed = s;
                out.push(r);
            }
            out
        };
        assert_eq!(run(1), run(1), "same seed must give the same sequence");
        assert_ne!(run(1), run(2), "different seeds must diverge");
    }

    #[test]
    fn full_period_has_no_early_repeat() {
        // A Lehmer generator with these constants has period 2^31-2. Confirm the
        // seed does not return to its start within a large sample, which would
        // indicate a broken modulus.
        let start = 1u32;
        let mut seed = start;
        for i in 0..100_000 {
            let (s, _) = next(seed);
            seed = s;
            assert_ne!(seed, start, "seed cycled after {i} steps");
            assert!(seed <= M as u32, "seed left the modulus at step {i}");
        }
    }
}
