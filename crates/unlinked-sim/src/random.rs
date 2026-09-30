//! Per-run, per-scalar-stream PRNG. No shared state and no solver-stage draws.
//! SplitMix64 plus Box–Muller: deterministic Unlinked sequences, not a claim of
//! compatibility with MATLAB's legacy v4 generator.
pub(crate) struct Generator(u64);
impl Generator {
    pub(crate) fn new(seed: u32) -> Self {
        Self(u64::from(seed))
    }
    pub(crate) fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^= z >> 31;
        // 52 random bits, with an exactly representable half-bin offset:
        // strictly inside (0,1), so Box–Muller never takes ln(0).
        ((z >> 12) as f64 + 0.5) * (1.0 / 4503599627370496.0)
    }
    pub(crate) fn normal(&mut self) -> f64 {
        let radius = (-2.0 * self.uniform().ln()).sqrt();
        let angle = std::f64::consts::TAU * self.uniform();
        radius * angle.cos()
    }
}
