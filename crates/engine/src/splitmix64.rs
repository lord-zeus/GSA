//! A small, deterministic, no-dependency PRNG: SplitMix64.
//!
//! Used to drive **all** random decisions in the engine so that, for a given
//! seed, spintax resolution is fully reproducible. This is essential for the
//! "all tests green" invariant we build the replica against.

/// SplitMix64 — a fast, well-distributed 64-bit PRNG with a trivial seed.
#[derive(Debug, Clone, Copy)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next 64-bit word.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `[0, n)` for `n > 0`. Returns 0 if `n == 0` (no panics
    /// in the hot path of rendering thousands of spintax fields).
    #[inline]
    pub fn range(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        usize::try_from(self.next_u64()).map(|v| (v as u128 % n as u128) as usize).unwrap_or(0)
    }

    /// A random boolean (~50/50). Used for `[optional]` inclusion.
    #[inline]
    pub fn bit(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

impl Default for SplitMix64 {
    fn default() -> Self {
        Self::new(0x1234_5678_9ABC_DEF0 | 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_stream_is_reproducible() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..50 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn range_is_within_bounds_and_spans() {
        let mut r = SplitMix64::new(7);
        let mut seen: usize = 0;
        for _ in 0..5000 {
            let v = r.range(10);
            assert!((0..10).contains(&v), "out of range: {}", v);
            seen |= 1 << v;
        }
        // all 10 buckets should have been hit by now
        assert_eq!(seen, (1 << 10) - 1);
        // zero must not panic
        assert_eq!(r.range(0), 0);
    }

    #[test]
    fn different_seeds_differ() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        let av = (0..8).map(|_| a.next_u64()).collect::<Vec<_>>();
        let bv = (0..8).map(|_| b.next_u64()).collect::<Vec<_>>();
        assert_ne!(av, bv);
    }
}
