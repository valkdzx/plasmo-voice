//! The microphone's filters, in Plasmo Voice's order: gain, then a noise gate (in place of
//! its RNNoise suppression: frames under a level fade out), a compressor and a limiter. Each
//! keeps its state across frames.

fn db(power: f32) -> f32 {
    10.0 * power.max(1e-10).log10()
}

/// The level of a frame, in dBFS.
pub fn level_db(frame: &[f32]) -> f32 {
    db(frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32)
}

/// Frames under `threshold_db` fade to silence over a few frames, and back when it is passed:
/// what stays of a room's hum between words.
pub struct NoiseGate {
    gain: f32,
}

impl Default for NoiseGate {
    fn default() -> Self {
        Self { gain: 1.0 }
    }
}

impl NoiseGate {
    pub fn process(&mut self, frame: &mut [f32], threshold_db: f32) {
        let target = if level_db(frame) > threshold_db { 1.0 } else { 0.0 };
        let n = frame.len().max(1) as f32;
        let start = self.gain;
        // Open fast (one frame), close slowly (five).
        let end = if target > start { target } else { (start - 0.2).max(target) };
        for (i, s) in frame.iter_mut().enumerate() {
            *s *= start + (end - start) * i as f32 / n;
        }
        self.gain = end;
    }
}

/// A feed-forward compressor: past -20 dBFS the level rises a quarter as fast, with 5 ms attack
/// and 100 ms release, then 6 dB of makeup.
pub struct Compressor {
    envelope: f32,
    attack: f32,
    release: f32,
}

impl Compressor {
    pub fn new(rate: u32) -> Self {
        let coefficient = |ms: f32| (-1.0 / (ms / 1000.0 * rate as f32)).exp();
        Self { envelope: 0.0, attack: coefficient(5.0), release: coefficient(100.0) }
    }
    pub fn process(&mut self, frame: &mut [f32]) {
        const THRESHOLD: f32 = -20.0;
        const RATIO: f32 = 4.0;
        const MAKEUP: f32 = 2.0;
        for s in frame.iter_mut() {
            let x = s.abs();
            let k = if x > self.envelope { self.attack } else { self.release };
            self.envelope = k * self.envelope + (1.0 - k) * x;
            let level = 20.0 * self.envelope.max(1e-6).log10();
            let reduction = if level > THRESHOLD { (level - THRESHOLD) * (1.0 - 1.0 / RATIO) } else { 0.0 };
            *s *= 10f32.powf(-reduction / 20.0) * MAKEUP;
        }
    }
}

/// Nothing past -1 dBFS: a soft knee from -6 dBFS up.
pub fn limit(frame: &mut [f32]) {
    const KNEE: f32 = 0.5;
    const CEILING: f32 = 0.89;
    for s in frame.iter_mut() {
        let x = s.abs();
        if x > KNEE {
            let over = (x - KNEE) / (CEILING - KNEE);
            *s = s.signum() * (KNEE + (CEILING - KNEE) * over.tanh());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_closes_on_hum_and_opens_on_speech() {
        let mut gate = NoiseGate::default();
        for _ in 0..10 {
            let mut hum = vec![0.001; 960];
            gate.process(&mut hum, -40.0);
        }
        let mut hum = vec![0.001; 960];
        gate.process(&mut hum, -40.0);
        assert!(hum.iter().all(|s| *s == 0.0));
        let mut speech = vec![0.3; 960];
        gate.process(&mut speech, -40.0);
        assert!((speech[959] - 0.3).abs() < 0.01);
    }

    #[test]
    fn compressor_tames_loud_frames() {
        let mut c = Compressor::new(48000);
        let mut loud = vec![0.9; 4800];
        c.process(&mut loud);
        // -1 dBFS in, about -20 + 19/4 + 6 = -9 dBFS out once the attack settled.
        let out = 20.0 * loud[4799].log10();
        assert!((-11.0..-7.0).contains(&out), "{out}");
    }

    #[test]
    fn limiter_holds_the_ceiling() {
        let mut f = vec![0.1, 0.6, 2.0, -5.0];
        limit(&mut f);
        assert_eq!(f[0], 0.1);
        assert!(f[1] > 0.5 && f[1] < 0.6);
        assert!(f[2] <= 0.89 && f[3] >= -0.89);
    }
}
