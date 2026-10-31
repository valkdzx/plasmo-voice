//! Plasmo Voice 2.x's packets, as its `protocol` module defines them (PacketTcpCodec,
//! PacketUdpCodec). TCP packets travel as plugin messages on `plasmo:voice/v2`: one byte of
//! packet id, then the packet. UDP packets go to the voice server's own socket behind a header.
//! Only the client's side is here: what the server sends is read, what the client sends is
//! written.
use crate::jio::{Malformed, Reader, Result, Writer};
use crate::md5::name_uuid;

/// The plugin channel of Plasmo Voice 2.x.
pub const CHANNEL: &str = "plasmo:voice/v2";
/// The channel the server marks its players' clients with (`FLAG_CHANNEL_STRING`).
pub const FLAG_CHANNEL: &str = "plasmo:voice/v2/installed";

/// `CodecInfo`: a codec's name (`opus`) and its parameters (`mode`, `bitrate`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodecInfo {
    pub name: String,
    pub params: Vec<(String, String)>,
}

impl CodecInfo {
    fn read(r: &mut Reader) -> Result<Self> {
        let name = r.utf()?;
        let n = r.len(128)?;
        let mut params = Vec::with_capacity(n);
        for _ in 0..n {
            params.push((r.utf()?, r.utf()?));
        }
        Ok(Self { name, params })
    }
    fn read_optional(r: &mut Reader) -> Result<Option<Self>> {
        if r.bool()? { Ok(Some(Self::read(r)?)) } else { Ok(None) }
    }
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// `McGameProfile`: a player's id and name (its properties are skipped).
#[derive(Clone, Debug, PartialEq)]
pub struct GameProfile {
    pub id: u128,
    pub name: String,
}

impl GameProfile {
    fn read(r: &mut Reader) -> Result<Self> {
        let id = r.uuid()?;
        let name = r.utf()?;
        for _ in 0..r.len(100)? {
            r.utf()?;
            r.utf()?;
            r.utf()?;
        }
        Ok(Self { id, name })
    }
}

/// `VoicePlayerInfo`: a player with voice chat, and whether they can be heard.
#[derive(Clone, Debug, PartialEq)]
pub struct VoicePlayer {
    pub id: u128,
    pub name: String,
    pub muted: bool,
    pub voice_disabled: bool,
    pub microphone_muted: bool,
}

impl VoicePlayer {
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(Self {
            id: r.uuid()?,
            name: r.utf()?,
            muted: r.bool()?,
            voice_disabled: r.bool()?,
            microphone_muted: r.bool()?,
        })
    }
}

/// `VoiceActivation`: a way to speak (proximity, a group's line…), with the distances it
/// allows and the codec it wants.
#[derive(Clone, Debug, PartialEq)]
pub struct Activation {
    pub id: u128,
    pub name: String,
    pub translation: String,
    /// Its icon, a texture path (`plasmovoice:textures/icons/microphone_group.png`).
    pub icon: String,
    pub distances: Vec<i32>,
    pub default_distance: i32,
    pub proximity: bool,
    pub transitive: bool,
    pub stereo_supported: bool,
    pub encoder: Option<CodecInfo>,
    pub weight: i32,
}

pub const PROXIMITY: &str = "proximity";

impl Activation {
    fn read(r: &mut Reader) -> Result<Self> {
        let name = r.utf()?;
        let translation = r.utf()?;
        let icon = r.utf()?;
        let n = r.len(64)?;
        let mut distances = Vec::with_capacity(n);
        for _ in 0..n {
            distances.push(r.i32()?);
        }
        let default_distance = valid_default(&distances, r.i32()?);
        Ok(Self {
            id: name_uuid(&format!("{name}_activation")),
            name,
            translation,
            icon,
            distances,
            default_distance,
            proximity: r.bool()?,
            transitive: r.bool()?,
            stereo_supported: r.bool()?,
            encoder: CodecInfo::read_optional(r)?,
            weight: r.i32()?,
        })
    }
    /// `Activation.checkDistance`: no list allows any distance, `[-1, max]` any from 1 to
    /// max, else one of the list.
    pub fn allows(&self, distance: i32) -> bool {
        match self.distances.as_slice() {
            [] => true,
            [-1, max] => (1..=*max).contains(&distance),
            list => list.contains(&distance),
        }
    }
}

/// `VoiceActivation.validateDefaultDistance`.
fn valid_default(distances: &[i32], default: i32) -> i32 {
    match distances {
        [] => 0,
        [-1, max] if (1..=*max).contains(&default) => default,
        [-1, max] => max / 2,
        list if list.contains(&default) => default,
        list => list[list.len() / 2],
    }
}

/// `VoiceSourceLine`: a channel sources play on, with its default volume.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceLine {
    pub id: u128,
    pub name: String,
    pub translation: String,
    pub icon: String,
    pub default_volume: f64,
    pub weight: i32,
}

impl SourceLine {
    fn read(r: &mut Reader) -> Result<Self> {
        let name = r.utf()?;
        let translation = r.utf()?;
        let icon = r.utf()?;
        let default_volume = r.f64()?.clamp(0.0, 1.0);
        let weight = r.i32()?;
        if r.bool()? {
            for _ in 0..r.len(i16::MAX as i32)? {
                GameProfile::read(r)?;
            }
        }
        Ok(Self { id: name_uuid(&format!("{name}_line")), name, translation, icon, default_volume, weight })
    }
}

/// Where a source plays from (`SourceInfo.Type`).
#[derive(Clone, Debug, PartialEq)]
pub enum SourceKind {
    /// A player, by their profile.
    Player(VoicePlayer),
    /// An entity, by its network id.
    Entity(i32),
    /// A fixed place.
    Static([f64; 3]),
    /// Not in the world: heard as it is, or from a place relative to the listener.
    Direct { sender: Option<GameProfile>, relative: Option<[f64; 3]> },
}

/// `SourceInfo` and its four kinds.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    pub addon_id: String,
    pub id: u128,
    pub name: Option<String>,
    pub state: u8,
    pub decoder: Option<CodecInfo>,
    pub stereo: bool,
    pub line_id: u128,
    pub icon_visible: bool,
    pub angle: i32,
    pub kind: SourceKind,
}

fn pos(r: &mut Reader) -> Result<[f64; 3]> {
    Ok([r.f64()?, r.f64()?, r.f64()?])
}

impl SourceInfo {
    fn read(r: &mut Reader) -> Result<Self> {
        let kind = r.utf()?;
        let addon_id = r.utf()?;
        let id = r.uuid()?;
        let name = r.nullable_utf()?;
        let state = r.u8()?;
        let decoder = CodecInfo::read_optional(r)?;
        let stereo = r.bool()?;
        let line_id = r.uuid()?;
        let icon_visible = r.bool()?;
        let angle = r.i32()?;
        let kind = match kind.as_str() {
            "PLAYER" => SourceKind::Player(VoicePlayer::read(r)?),
            "ENTITY" => SourceKind::Entity(r.i32()?),
            "STATIC" => {
                let at = pos(r)?;
                let _look = pos(r)?;
                SourceKind::Static(at)
            }
            "DIRECT" => {
                let sender = if r.bool()? { Some(GameProfile::read(r)?) } else { None };
                let relative = if r.bool()? { Some(pos(r)?) } else { None };
                let _camera_relative = r.bool()?;
                SourceKind::Direct { sender, relative }
            }
            _ => return Err(Malformed("unknown source type")),
        };
        Ok(Self { addon_id, id, name, state, decoder, stereo, line_id, icon_visible, angle, kind })
    }
}

/// `ConfigPacket`: the voice server's settings for this player.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub server_id: u128,
    pub sample_rate: i32,
    pub mtu_size: i32,
    pub capture_encoder: Option<CodecInfo>,
    /// The cipher's name and its key, encrypted with this client's RSA key.
    pub encryption: Option<(String, Vec<u8>)>,
    pub lines: Vec<SourceLine>,
    pub activations: Vec<Activation>,
    pub permissions: Vec<(String, bool)>,
}

fn permissions(r: &mut Reader) -> Result<Vec<(String, bool)>> {
    let n = r.len(i8::MAX as i32)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push((r.utf()?, r.bool()?));
    }
    Ok(out)
}

impl Config {
    fn read(r: &mut Reader) -> Result<Self> {
        let server_id = r.uuid()?;
        let sample_rate = r.i32()?;
        let mtu_size = r.i32()?;
        let capture_encoder = CodecInfo::read_optional(r)?;
        let encryption = if r.bool()? {
            let algorithm = r.utf()?;
            let n = r.int_in(1, 2048)? as usize;
            Some((algorithm, r.bytes(n)?.to_vec()))
        } else {
            None
        };
        let mut lines = Vec::new();
        for _ in 0..r.len(i8::MAX as i32)? {
            lines.push(SourceLine::read(r)?);
        }
        let mut activations = Vec::new();
        for _ in 0..r.len(i8::MAX as i32)? {
            activations.push(Activation::read(r)?);
        }
        let permissions = permissions(r)?;
        // A PlayerIconConfig follows; the client reads it only if it is there, and so do we
        // (we draw no icons).
        Ok(Self {
            server_id,
            sample_rate,
            mtu_size,
            capture_encoder,
            encryption,
            lines,
            activations,
            permissions,
        })
    }
}

/// A TCP packet from the server (`PacketDirection.CLIENT`).
#[derive(Clone, Debug, PartialEq)]
pub enum Clientbound {
    /// `ConnectionPacket`: the UDP secret and the voice server's address (0.0.0.0: the game's).
    Connection { secret: u128, ip: String, port: i32 },
    /// `PlayerInfoRequestPacket`: answer with `player_info`.
    PlayerInfoRequest,
    Config(Box<Config>),
    ConfigPlayerInfo(Vec<(String, bool)>),
    PlayerList(Vec<VoicePlayer>),
    PlayerInfoUpdate(VoicePlayer),
    PlayerDisconnect(u128),
    SourceInfo(Box<SourceInfo>),
    /// `SelfSourceInfoPacket`: a source that carries this player's own voice.
    SelfSourceInfo { source: Box<SourceInfo>, player: u128, activation: u128, sequence: i64 },
    SourceAudioEnd { source: u128, sequence: i64 },
    ActivationRegister(Box<Activation>),
    ActivationUnregister(u128),
    SourceLineRegister(SourceLine),
    SourceLineUnregister(u128),
    /// `AnimatedActionBarPacket`: a JSON text component for the action bar.
    ActionBar(String),
    /// `LanguagePacket`: the server's translations of its keys (`pv.activation.groups`…).
    Language(Vec<(String, String)>),
    /// `DistanceVisualizePacket`: show a distance, around a place or the player.
    DistanceVisualize { radius: i32, color: i32, at: Option<[f64; 3]> },
    /// A packet this client has nothing to do with (languages, distance visuals, the players
    /// of group lines).
    Ignored(u8),
}

impl Clientbound {
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let id = r.u8()?;
        Ok(match id {
            0x01 => Self::Connection { secret: r.uuid()?, ip: r.utf()?, port: r.i32()? },
            0x02 => Self::PlayerInfoRequest,
            0x03 => Self::Config(Box::new(Config::read(&mut r)?)),
            0x04 => Self::ConfigPlayerInfo(permissions(&mut r)?),
            0x06 => {
                let _name = r.utf()?;
                let n = r.len(i16::MAX as i32)?;
                let mut entries = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    entries.push((r.utf()?, r.utf()?));
                }
                Self::Language(entries)
            }
            0x0E => Self::DistanceVisualize {
                radius: r.i32()?,
                color: r.i32()?,
                at: if r.bool()? { Some(pos(&mut r)?) } else { None },
            },
            0x07 => {
                let n = r.len(i16::MAX as i32)?;
                let mut players = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    players.push(VoicePlayer::read(&mut r)?);
                }
                Self::PlayerList(players)
            }
            0x08 => Self::PlayerInfoUpdate(VoicePlayer::read(&mut r)?),
            0x09 => Self::PlayerDisconnect(r.uuid()?),
            0x10 => Self::SourceInfo(Box::new(SourceInfo::read(&mut r)?)),
            0x11 => Self::SelfSourceInfo {
                source: Box::new(SourceInfo::read(&mut r)?),
                player: r.uuid()?,
                activation: r.uuid()?,
                sequence: r.i64()?,
            },
            0x12 => Self::SourceAudioEnd { source: r.uuid()?, sequence: r.i64()? },
            0x13 => Self::ActivationRegister(Box::new(Activation::read(&mut r)?)),
            0x14 => Self::ActivationUnregister(r.uuid()?),
            0x15 => Self::SourceLineRegister(SourceLine::read(&mut r)?),
            0x16 => Self::SourceLineUnregister(r.uuid()?),
            0x1A => Self::ActionBar(r.utf()?),
            other => Self::Ignored(other),
        })
    }
}

/// The TCP packets this client sends (`PacketDirection.SERVER`), each as its payload.
pub mod serverbound {
    use crate::jio::Writer;

    /// `PlayerInfoPacket`: this client's versions and RSA public key (X.509), sent when the
    /// server asks. The server needs the same major version as its own.
    pub fn player_info(
        voice_disabled: bool,
        microphone_muted: bool,
        minecraft_version: &str,
        version: &str,
        public_key: &[u8],
    ) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x0A).bool(voice_disabled).bool(microphone_muted);
        w.utf(minecraft_version).utf(version);
        w.i32(public_key.len() as i32).bytes(public_key);
        w.data
    }
    /// `LanguageRequestPacket`: the client's language (`ru_ru`), for `LanguagePacket`.
    pub fn language_request(language: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x05).utf(language);
        w.data
    }
    /// `PlayerStatePacket`.
    pub fn player_state(voice_disabled: bool, microphone_muted: bool) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x0B).bool(voice_disabled).bool(microphone_muted);
        w.data
    }
    /// `PlayerAudioEndPacket`: the last sequence number of a stream of an activation.
    pub fn audio_end(sequence: i64, activation: u128, distance: i16) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x0C).i64(sequence).uuid(activation).i16(distance);
        w.data
    }
    /// `PlayerActivationDistancesPacket`.
    pub fn activation_distances(distances: &[(u128, i32)]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x0D).i32(distances.len() as i32);
        for (activation, distance) in distances {
            w.uuid(*activation).i32(*distance);
        }
        w.data
    }
    /// `SourceInfoRequestPacket`.
    pub fn source_info_request(source: u128) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x0F).uuid(source);
        w.data
    }
}

/// `PacketUdpCodec`'s magic number, in front of every UDP packet.
pub const UDP_MAGIC: i32 = 0x4e9004e9;

/// A UDP packet from the voice server.
#[derive(Clone, Debug, PartialEq)]
pub enum UdpIn {
    /// `PingPacket`: the server's keep-alive; answer with `ping`.
    Ping,
    /// `SourceAudioPacket`: one frame of a source, encrypted and encoded.
    SourceAudio { sequence: i64, data: Vec<u8>, source: u128, state: u8, distance: i16 },
    /// `SelfAudioInfoPacket`, `CustomPacket` and the rest.
    Other,
}

impl UdpIn {
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        if r.i32()? != UDP_MAGIC {
            return Err(Malformed("not a Plasmo Voice packet"));
        }
        let kind = r.u8()?;
        let _secret = r.uuid()?;
        let _timestamp = r.i64()?;
        Ok(match kind {
            1 => Self::Ping,
            3 => {
                let sequence = r.i64()?;
                let n = r.int_in(1, 2048)? as usize;
                let data = r.bytes(n)?.to_vec();
                Self::SourceAudio {
                    sequence,
                    data,
                    source: r.uuid()?,
                    state: r.u8()?,
                    distance: r.i16()?,
                }
            }
            _ => Self::Other,
        })
    }
}

fn udp(kind: u8, secret: u128, now_ms: i64) -> Writer {
    let mut w = Writer::new();
    w.i32(UDP_MAGIC).u8(kind).uuid(secret).i64(now_ms);
    w
}

/// `PingPacket`. Until the server answers, the client names the address it sends to, which
/// lets a proxy route it; the answer to the server's pings has none.
pub fn udp_ping(secret: u128, now_ms: i64, to: Option<(&str, u16)>) -> Vec<u8> {
    let mut w = udp(1, secret, now_ms);
    w.i64(now_ms);
    if let Some((ip, port)) = to {
        w.utf(ip).u16(port);
    }
    w.data
}

/// `PlayerAudioPacket`: one encoded (and maybe encrypted) frame of an activation.
pub fn udp_player_audio(
    secret: u128,
    now_ms: i64,
    sequence: i64,
    data: &[u8],
    activation: u128,
    distance: i16,
    stereo: bool,
) -> Vec<u8> {
    let mut w = udp(2, secret, now_ms);
    w.i64(sequence).i32(data.len() as i32).bytes(data);
    w.uuid(activation).i16(distance).bool(stereo);
    w.data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activation(w: &mut Writer, name: &str, distances: &[i32], default: i32) {
        w.utf(name).utf("pv.activation.proximity").utf("plasmovoice:textures/icons/microphone.png");
        w.i32(distances.len() as i32);
        for d in distances {
            w.i32(*d);
        }
        w.i32(default).bool(true).bool(true).bool(false);
        w.bool(true).utf("opus").i32(2).utf("mode").utf("VOIP").utf("bitrate").utf("-1000");
        w.i32(1);
    }

    #[test]
    fn reads_a_config() {
        let mut w = Writer::new();
        w.u8(0x03).uuid(42).i32(48000).i32(1024).bool(false);
        w.bool(true).utf("AES/CBC/PKCS5Padding").i32(3).bytes(&[1, 2, 3]);
        w.i32(1).utf("proximity").utf("pv.line.proximity").utf("icon").bytes(&1.0f64.to_be_bytes()).i32(1).bool(false);
        w.i32(1);
        activation(&mut w, "proximity", &[8, 16, 32], 20);
        w.i32(1).utf("pv.allow.freecam").bool(true);
        w.i32(0).bytes(&[0; 24]);
        let Clientbound::Config(c) = Clientbound::decode(&w.data).unwrap() else { panic!() };
        assert_eq!(c.sample_rate, 48000);
        assert_eq!(c.encryption, Some(("AES/CBC/PKCS5Padding".into(), vec![1, 2, 3])));
        assert_eq!(c.lines[0].id, name_uuid("proximity_line"));
        let a = &c.activations[0];
        assert_eq!(a.id, name_uuid("proximity_activation"));
        // 20 is not in the list: the middle one is the default.
        assert_eq!(a.default_distance, 16);
        assert_eq!(a.encoder.as_ref().unwrap().param("bitrate"), Some("-1000"));
        assert!(a.allows(32) && !a.allows(20));
        assert_eq!(c.permissions, [("pv.allow.freecam".to_string(), true)]);
    }

    #[test]
    fn reads_sources() {
        let mut w = Writer::new();
        w.u8(0x10).utf("PLAYER").utf("plasmovoice").uuid(7).bool(false).u8(3).bool(false).bool(false);
        w.uuid(9).bool(true).i32(360).uuid(11).utf("Steve").bool(false).bool(false).bool(true);
        let Clientbound::SourceInfo(s) = Clientbound::decode(&w.data).unwrap() else { panic!() };
        assert_eq!((s.id, s.state, s.line_id), (7, 3, 9));
        assert!(matches!(&s.kind, SourceKind::Player(p) if p.name == "Steve" && p.microphone_muted));
    }

    #[test]
    fn udp_round_trip() {
        let mut w = udp(3, 5, 1000);
        w.i64(12).i32(3).bytes(&[1, 2, 3]).uuid(77).u8(1).i16(16);
        assert_eq!(
            UdpIn::decode(&w.data).unwrap(),
            UdpIn::SourceAudio { sequence: 12, data: vec![1, 2, 3], source: 77, state: 1, distance: 16 }
        );
        let ping = udp_ping(5, 1000, Some(("127.0.0.1", 60606)));
        assert_eq!(UdpIn::decode(&ping).unwrap(), UdpIn::Ping);
        assert_eq!(&ping[..5], &[0x4e, 0x90, 0x04, 0xe9, 1]);
        assert!(UdpIn::decode(&[0, 0, 0, 0, 1]).is_err());
    }

    #[test]
    fn writes_player_info() {
        let p = serverbound::player_info(false, true, "26.3", "2.2.0", &[9, 9]);
        assert_eq!(p[..3], [0x0A, 0, 1]);
        assert_eq!(&p[p.len() - 6..], &[0, 0, 0, 2, 9, 9]);
    }
}
