//! The codecs a server can name (`CodecInfo`): Opus, or none, when frames travel as 16-bit
//! little-endian PCM (`AudioUtil.shortsToBytes`).
use crate::opus::{Application, OpusDecoder, OpusEncoder};
use crate::proto::CodecInfo;

/// `OpusEncoderInfo`'s bitrate: -1000 lets Opus choose, -1 is the most, else 500 to 512 000.
fn bitrate(info: &CodecInfo, rate: i32, frame: usize) -> i32 {
    match info.param("bitrate").and_then(|b| b.parse::<i32>().ok()).unwrap_or(-1000) {
        // OPUS_BITRATE_MAX
        -1 => 510_000,
        // OPUS_AUTO: libopus' user_bitrate_to_bitrate for one channel.
        b if b < 0 => 60 * rate / frame as i32 + rate,
        b => b.clamp(500, 512_000),
    }
}

pub struct Encoder {
    opus: Option<OpusEncoder>,
    info: Option<CodecInfo>,
    rate: i32,
    frame: usize,
    out: Vec<u8>,
}

impl Encoder {
    /// An encoder of `frame`-sample mono frames at `rate` for `info`. An unknown codec is an
    /// error: the server would forward frames nobody can play.
    pub fn new(info: Option<&CodecInfo>, rate: i32, frame: usize) -> Result<Self, String> {
        let opus = match info {
            None => None,
            Some(i) if i.name == "opus" => {
                let application = match i.param("mode") {
                    Some("AUDIO") => Application::Audio,
                    Some("RESTRICTED_LOWDELAY") => Application::RestrictedLowDelay,
                    _ => Application::Voip,
                };
                let mut e = OpusEncoder::new(rate, 1, application).map_err(|e| format!("Opus: {e:?}"))?;
                e.bitrate_bps = bitrate(i, rate, frame);
                Some(e)
            }
            Some(i) => return Err(format!("the server wants codec {}, which this client lacks", i.name)),
        };
        Ok(Self { opus, info: info.cloned(), rate, frame, out: vec![0; 4000] })
    }
    /// One frame of `frame` samples in -1..1.
    pub fn encode(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
        match &mut self.opus {
            Some(e) => {
                let n = e.encode(pcm, self.frame, &mut self.out).ok()?;
                Some(self.out[..n].to_vec())
            }
            None => Some(
                pcm.iter()
                    .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
                    .collect(),
            ),
        }
    }
    /// A fresh stream (`AudioEncoder.reset`, at the end of each one).
    pub fn reset(&mut self) {
        if let Ok(e) = Self::new(self.info.as_ref(), self.rate, self.frame) {
            *self = e;
        }
    }
}

pub struct Decoder {
    opus: Option<OpusDecoder>,
    channels: usize,
    frame: usize,
}

impl Decoder {
    pub fn new(info: Option<&CodecInfo>, rate: i32, frame: usize, stereo: bool) -> Result<Self, String> {
        let channels = if stereo { 2 } else { 1 };
        let opus = match info {
            None => None,
            Some(i) if i.name == "opus" => {
                Some(OpusDecoder::new(rate, channels).map_err(|e| format!("Opus: {e:?}"))?)
            }
            Some(i) => return Err(format!("a source uses codec {}, which this client lacks", i.name)),
        };
        Ok(Self { opus, channels, frame })
    }
    pub fn channels(&self) -> usize {
        self.channels
    }
    /// One packet's samples, interleaved; an empty packet conceals a lost one (Opus only).
    pub fn decode(&mut self, data: &[u8]) -> Option<Vec<f32>> {
        match &mut self.opus {
            Some(d) => {
                // A packet holds up to 120 ms; a lost one is concealed as one frame.
                let max = if data.is_empty() { self.frame } else { self.frame * 6 };
                let mut out = vec![0.0; max * self.channels];
                let n = d.decode(data, max, &mut out).ok()?;
                out.truncate(n * self.channels);
                Some(out)
            }
            None if data.is_empty() => None,
            None => Some(
                data.chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opus(mode: &str) -> CodecInfo {
        CodecInfo {
            name: "opus".into(),
            params: vec![("mode".into(), mode.into()), ("bitrate".into(), "-1000".into())],
        }
    }

    #[test]
    fn opus_round_trip_keeps_a_tone() {
        let info = opus("VOIP");
        let mut enc = Encoder::new(Some(&info), 48000, 960).unwrap();
        let mut dec = Decoder::new(Some(&info), 48000, 960, false).unwrap();
        let mut energy = 0.0;
        for f in 0..20 {
            let pcm: Vec<f32> = (0..960)
                .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * (f * 960 + i) as f32 / 48000.0).sin())
                .collect();
            let packet = enc.encode(&pcm).unwrap();
            assert!(packet.len() < 400, "{}", packet.len());
            let out = dec.decode(&packet).unwrap();
            assert_eq!(out.len(), 960);
            if f > 5 {
                energy += out.iter().map(|s| s * s).sum::<f32>() / 960.0;
            }
        }
        // A 0.5 sine has a power of 0.125; the codec keeps most of it.
        let power = energy / 14.0;
        assert!((0.06..0.2).contains(&power), "{power}");
        // A lost packet is concealed.
        assert_eq!(dec.decode(&[]).unwrap().len(), 960);
    }

    #[test]
    fn raw_pcm_is_little_endian_shorts() {
        let mut enc = Encoder::new(None, 48000, 2).unwrap();
        assert_eq!(enc.encode(&[0.5, -1.0]).unwrap(), [0xFF, 0x3F, 0x01, 0x80]);
        let mut dec = Decoder::new(None, 48000, 2, false).unwrap();
        assert_eq!(dec.decode(&[0x00, 0x40, 0x00, 0x80]).unwrap(), [0.5, -1.0]);
    }

    #[test]
    fn unknown_codec_is_refused() {
        let info = CodecInfo { name: "speex".into(), params: vec![] };
        assert!(Encoder::new(Some(&info), 48000, 960).is_err());
    }
}
