//! Opus as Simple Voice Chat uses it: 48 kHz mono, 20 ms frames, the application the server
//! names.
use crate::opus::{Application, OpusDecoder, OpusEncoder};
use crate::proto::Codec;

pub const RATE: i32 = 48_000;
/// 20 ms.
pub const FRAME: usize = 960;

pub struct Encoder {
    opus: OpusEncoder,
    codec: Codec,
    out: Vec<u8>,
}

impl Encoder {
    pub fn new(codec: Codec) -> Result<Self, String> {
        let application = match codec {
            Codec::Voip => Application::Voip,
            Codec::Audio => Application::Audio,
            Codec::RestrictedLowDelay => Application::RestrictedLowDelay,
        };
        let mut opus = OpusEncoder::new(RATE, 1, application).map_err(|e| format!("Opus: {e:?}"))?;
        // libopus' automatic bitrate for one channel (OPUS_AUTO).
        opus.bitrate_bps = 60 * RATE / FRAME as i32 + RATE;
        Ok(Self { opus, codec, out: vec![0; 1500] })
    }
    /// One frame of `FRAME` samples in -1..1.
    pub fn encode(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
        let n = self.opus.encode(pcm, FRAME, &mut self.out).ok()?;
        Some(self.out[..n].to_vec())
    }
    /// A fresh stream, after each one ends.
    pub fn reset(&mut self) {
        if let Ok(e) = Self::new(self.codec) {
            *self = e;
        }
    }
}

pub struct Decoder {
    opus: OpusDecoder,
}

impl Decoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self { opus: OpusDecoder::new(RATE, 1).map_err(|e| format!("Opus: {e:?}"))? })
    }
    /// One packet's samples; an empty packet conceals a lost one.
    pub fn decode(&mut self, data: &[u8]) -> Option<Vec<f32>> {
        // A packet holds up to 120 ms; a lost one is concealed as one frame.
        let max = if data.is_empty() { FRAME } else { FRAME * 6 };
        let mut out = vec![0.0; max];
        let n = self.opus.decode(data, max, &mut out).ok()?;
        out.truncate(n);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_a_tone() {
        let mut e = Encoder::new(Codec::Voip).unwrap();
        let mut d = Decoder::new().unwrap();
        let mut energy = 0.0;
        for f in 0..10 {
            let pcm: Vec<f32> = (0..FRAME)
                .map(|i| ((f * FRAME + i) as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.5)
                .collect();
            let packet = e.encode(&pcm).unwrap();
            assert!(!packet.is_empty() && packet.len() < 1024);
            let out = d.decode(&packet).unwrap();
            assert_eq!(out.len(), FRAME);
            if f > 3 {
                energy += out.iter().map(|s| s * s).sum::<f32>();
            }
        }
        assert!(energy > 100.0, "energy {energy}");
        assert_eq!(d.decode(&[]).map(|p| p.len()), Some(FRAME));
    }
}
