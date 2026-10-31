//! What the speakers play: every source's decoded frames, queued against network jitter,
//! resampled to the output device and mixed with its left and right gains, which the game
//! sets each tick from where the source is.
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Frames buffered before a source starts (or starts again after running dry): 40 ms of
/// jitter absorbed for 40 ms of delay.
const START_MS: usize = 40;
/// More than this queued and the oldest go: a source that fell behind catches up.
const MAX_MS: usize = 400;

/// A stereo source's gain over a mono one's: the game's gains are equal-power for mono, and
/// OpenAL plays stereo unpanned.
const STEREO: f32 = std::f32::consts::SQRT_2;

struct Voice {
    channels: usize,
    /// Interleaved samples at the server's rate.
    queue: VecDeque<f32>,
    playing: bool,
    /// The stream ended (`SourceAudioEndPacket`): play out what is left, without waiting.
    ended: bool,
    /// Read position into the queue's first frame, in frames.
    position: f64,
    gains: [f32; 2],
    target: [f32; 2],
    distance: i16,
    heard: Instant,
}

pub struct Mixer {
    /// The voice server's sample rate.
    pub rate: u32,
    /// The player's volume, 0 to 2.
    pub master: f32,
    voices: HashMap<u128, Voice>,
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self { rate, master: 1.0, voices: HashMap::new() }
    }
    /// A decoded packet of `source`; `distance` is how far it carries.
    pub fn push(&mut self, source: u128, channels: usize, samples: &[f32], distance: i16) {
        let rate = self.rate as usize;
        let voice = self.voices.entry(source).or_insert_with(|| Voice {
            channels,
            queue: VecDeque::new(),
            playing: false,
            ended: false,
            position: 0.0,
            // Silent until the game places it.
            gains: [0.0; 2],
            target: [crate::engine::CENTRE; 2],
            distance,
            heard: Instant::now(),
        });
        if voice.channels != channels {
            voice.queue.clear();
            voice.channels = channels;
        }
        voice.ended = false;
        voice.distance = distance;
        voice.heard = Instant::now();
        voice.queue.extend(samples);
        let max = rate * MAX_MS / 1000 * channels;
        let excess = voice.queue.len().saturating_sub(max);
        voice.queue.drain(..excess);
        if !voice.playing && voice.queue.len() >= rate * START_MS / 1000 * channels {
            voice.playing = true;
        }
    }
    pub fn end(&mut self, source: u128) {
        if let Some(v) = self.voices.get_mut(&source) {
            v.ended = true;
            v.playing = true;
        }
    }
    pub fn clear(&mut self) {
        self.voices.clear();
    }
    /// Sources heard within `within`, with how far each carries.
    pub fn heard(&self, within: Duration) -> Vec<(u128, i16)> {
        self.voices
            .iter()
            .filter(|(_, v)| v.heard.elapsed() < within)
            .map(|(id, v)| (*id, v.distance))
            .collect()
    }
    pub fn set_gains(&mut self, source: u128, gains: [f32; 2]) {
        if let Some(v) = self.voices.get_mut(&source) {
            v.target = gains;
        }
    }
    /// Fill `out` (interleaved, `channels` per frame, at `rate`) with every source.
    pub fn render(&mut self, out: &mut [f32], channels: usize, rate: u32) {
        out.fill(0.0);
        let frames = out.len() / channels.max(1);
        let step = self.rate as f64 / rate as f64;
        self.voices.retain(|_, v| !(v.ended && v.queue.is_empty()) && v.heard.elapsed() < Duration::from_secs(30));
        for v in self.voices.values_mut() {
            if !v.playing {
                continue;
            }
            let c = v.channels;
            let available = v.queue.len() / c;
            // Each gain glides to its target with an 80 ms time constant, sample by sample:
            // the game moves targets 20 times a second, and steps would crackle.
            let glide = 1.0 - (-1.0 / (0.08 * rate as f32)).exp();
            let target = v.target;
            for f in 0..frames {
                let i = v.position as usize;
                if i + 1 >= available {
                    // Dry: wait for more (or, ended, go).
                    v.playing = v.ended;
                    break;
                }
                let t = (v.position - i as f64) as f32;
                let sample = |ch: usize| {
                    let a = v.queue[i * c + ch];
                    let b = v.queue[(i + 1) * c + ch];
                    a + (b - a) * t
                };
                let (l, r) = if c == 1 { (sample(0), sample(0)) } else { (sample(0), sample(1)) };
                v.gains[0] += (target[0] - v.gains[0]) * glide;
                v.gains[1] += (target[1] - v.gains[1]) * glide;
                let [gl, gr] = if c == 1 { v.gains } else { v.gains.map(|g| g * STEREO) };
                let (l, r) = (l * gl, r * gr);
                match channels {
                    1 => out[f] += (l + r) * 0.5,
                    _ => {
                        out[f * channels] += l;
                        out[f * channels + 1] += r;
                    }
                }
                v.position += step;
            }
            let used = (v.position as usize).min(available);
            v.queue.drain(..used * c);
            v.position -= used as f64;
            if v.ended && v.queue.len() <= c {
                v.queue.clear();
            }
        }
        for s in out.iter_mut() {
            *s = (*s * self.master).clamp(-1.0, 1.0);
        }
    }
}

/// A linear resampler for a stream: what it is given, block by block, at `from` Hz comes out
/// at `to` Hz.
pub struct Resampler {
    step: f64,
    position: f64,
    pending: Vec<f32>,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, position: 0.0, pending: Vec::new() }
    }
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        self.pending.extend_from_slice(input);
        while (self.position as usize) + 1 < self.pending.len() {
            let i = self.position as usize;
            let t = (self.position - i as f64) as f32;
            out.push(self.pending[i] + (self.pending[i + 1] - self.pending[i]) * t);
            self.position += self.step;
        }
        let used = (self.position as usize).min(self.pending.len().saturating_sub(1));
        self.pending.drain(..used);
        self.position -= used as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_its_buffer_then_plays() {
        let mut m = Mixer::new(48000);
        m.push(1, 1, &[0.5; 960], 16);
        m.set_gains(1, [1.0, 0.0]);
        let mut out = vec![0.0; 2 * 480];
        m.render(&mut out, 2, 48000);
        assert!(out.iter().all(|s| *s == 0.0), "20 ms is under the start buffer");
        m.push(1, 1, &[0.5; 14400], 16);
        // A quarter second on, the gains have glided to their targets (96 %): left, nothing
        // right.
        let mut out = vec![0.0; 2 * 12000];
        m.render(&mut out, 2, 48000);
        assert!((out[2 * 11999] - 0.48).abs() < 0.01, "{}", out[2 * 11999]);
        assert_eq!(out[2 * 11999 + 1], 0.0);
    }

    #[test]
    fn ended_source_plays_out_and_goes() {
        let mut m = Mixer::new(48000);
        m.push(1, 1, &[0.25; 100], 16);
        m.end(1);
        let mut out = vec![0.0; 200];
        m.render(&mut out, 1, 48000);
        assert!(out[50] > 0.0);
        m.render(&mut out, 1, 48000);
        assert!(m.heard(Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn resampler_keeps_time() {
        let mut r = Resampler::new(44100, 48000);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&[0.0; 441], &mut out);
        }
        // One second in, one second out (give or take the sample held back).
        assert!((47990..=48000).contains(&out.len()), "{}", out.len());
    }
}
