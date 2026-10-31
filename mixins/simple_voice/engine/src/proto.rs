//! Simple Voice Chat's protocol, as its 2.6 releases speak it (compatibility version 20):
//! the plugin messages on the game connection (`voicechat:*`) and the voice server's UDP
//! packets, AES/GCM-encrypted with the secret the server hands out.
use crate::buf::{Malformed, Reader, Result, Writer};
use crate::gcm::Gcm;

/// `Voicechat.COMPATIBILITY_VERSION` of Simple Voice Chat 2.6: the server answers only a
/// client of its own version.
pub const COMPATIBILITY_VERSION: i32 = 20;

/// The plugin channels, by what they carry.
pub mod channel {
    pub const REQUEST_SECRET: &str = "voicechat:request_secret";
    pub const SECRET: &str = "voicechat:secret";
    pub const UPDATE_STATE: &str = "voicechat:update_state";
    pub const STATE: &str = "voicechat:state";
    pub const STATES: &str = "voicechat:states";
    pub const REMOVE_STATE: &str = "voicechat:remove_state";
    pub const ADD_GROUP: &str = "voicechat:add_group";
    pub const REMOVE_GROUP: &str = "voicechat:remove_group";
    pub const JOINED_GROUP: &str = "voicechat:joined_group";
    pub const SET_GROUP: &str = "voicechat:set_group";
    pub const CREATE_GROUP: &str = "voicechat:create_group";
    pub const LEAVE_GROUP: &str = "voicechat:leave_group";
    pub const ADD_CATEGORY: &str = "voicechat:add_category";
    pub const REMOVE_CATEGORY: &str = "voicechat:remove_category";

    /// Every channel, for `minecraft:register`: Paper sends a plugin's messages only on
    /// channels the client registered.
    pub const ALL: &[&str] = &[
        REQUEST_SECRET,
        SECRET,
        UPDATE_STATE,
        STATE,
        STATES,
        REMOVE_STATE,
        ADD_GROUP,
        REMOVE_GROUP,
        JOINED_GROUP,
        SET_GROUP,
        CREATE_GROUP,
        LEAVE_GROUP,
        ADD_CATEGORY,
        REMOVE_CATEGORY,
    ];
}

/// The longest group name or password the server takes.
pub const MAX_GROUP_TEXT: usize = 512;

/// The Opus application the server wants (`ServerConfig.Codec`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Voip,
    Audio,
    RestrictedLowDelay,
}

/// `SecretPacket`: how to reach and talk to the voice server.
#[derive(Clone, Debug, PartialEq)]
pub struct Secret {
    pub secret: [u8; 16],
    pub port: i32,
    pub player: u128,
    pub codec: Codec,
    pub mtu: i32,
    pub distance: f64,
    /// Milliseconds between the server's keep-alives.
    pub keep_alive: i32,
    pub groups_enabled: bool,
    /// Where the voice server is, when not the game server's host: a port, a host or both.
    pub voice_host: String,
    pub allow_recording: bool,
}

/// `PlayerState`: a player's voice chat as everyone sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub id: u128,
    pub name: String,
    /// Their voice chat is off.
    pub disabled: bool,
    /// They aren't connected to the voice server (or don't have the mod).
    pub disconnected: bool,
    pub group: Option<u128>,
}

impl PlayerState {
    fn read(r: &mut Reader) -> Result<Self> {
        let disabled = r.bool()?;
        let disconnected = r.bool()?;
        let id = r.uuid()?;
        let name = r.utf(32767)?;
        let group = if r.bool()? { Some(r.uuid()?) } else { None };
        Ok(Self { id, name, disabled, disconnected, group })
    }
}

/// How a group sounds to the players outside it (`Group.Type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupType {
    /// Members hear each other anywhere, and the players near them too.
    Normal,
    /// Like normal, and the players near them hear the group.
    Open,
    /// Members hear only each other.
    Isolated,
}

impl GroupType {
    fn from_short(v: i16) -> Self {
        match v {
            1 => Self::Open,
            2 => Self::Isolated,
            _ => Self::Normal,
        }
    }
    pub fn to_short(self) -> i16 {
        match self {
            Self::Normal => 0,
            Self::Open => 1,
            Self::Isolated => 2,
        }
    }
}

/// `ClientGroup`.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: u128,
    pub name: String,
    pub has_password: bool,
    pub persistent: bool,
    pub hidden: bool,
    pub kind: GroupType,
}

impl Group {
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(Self {
            id: r.uuid()?,
            name: r.utf(MAX_GROUP_TEXT)?,
            has_password: r.bool()?,
            persistent: r.bool()?,
            hidden: r.bool()?,
            kind: GroupType::from_short(r.i16()?),
        })
    }
}

/// A plugin message from the server.
#[derive(Clone, Debug, PartialEq)]
pub enum Clientbound {
    Secret(Box<Secret>),
    State(PlayerState),
    States(Vec<PlayerState>),
    RemoveState(u128),
    AddGroup(Group),
    RemoveGroup(u128),
    /// The group this player is in now (None: none), and whether a join failed on its password.
    JoinedGroup { group: Option<u128>, wrong_password: bool },
    /// Volume categories of add-ons: not used by this client.
    Ignored,
}

impl Clientbound {
    /// The message `data` on `channel`; None for a channel that isn't Simple Voice Chat's.
    pub fn decode(channel: &str, data: &[u8]) -> Option<Result<Self>> {
        let mut r = Reader::new(data);
        let r = &mut r;
        let packet = match channel {
            channel::SECRET => (|| {
                let secret: [u8; 16] = r.bytes(16)?.try_into().unwrap();
                let port = r.i32()?;
                let player = r.uuid()?;
                let codec = match r.u8()? {
                    0 => Codec::Voip,
                    1 => Codec::Audio,
                    2 => Codec::RestrictedLowDelay,
                    _ => return Err(Malformed("unknown codec")),
                };
                Ok(Self::Secret(Box::new(Secret {
                    secret,
                    port,
                    player,
                    codec,
                    mtu: r.i32()?,
                    distance: r.f64()?,
                    keep_alive: r.i32()?,
                    groups_enabled: r.bool()?,
                    voice_host: r.utf(32767)?,
                    allow_recording: r.bool()?,
                })))
            })(),
            channel::STATE => PlayerState::read(r).map(Self::State),
            channel::STATES => (|| {
                let n = r.i32()?;
                if n < 0 {
                    return Err(Malformed("negative count"));
                }
                let mut list = Vec::with_capacity((n as usize).min(1024));
                for _ in 0..n {
                    list.push(PlayerState::read(r)?);
                }
                Ok(Self::States(list))
            })(),
            channel::REMOVE_STATE => r.uuid().map(Self::RemoveState),
            channel::ADD_GROUP => Group::read(r).map(Self::AddGroup),
            channel::REMOVE_GROUP => r.uuid().map(Self::RemoveGroup),
            channel::JOINED_GROUP => (|| {
                let group = if r.bool()? { Some(r.uuid()?) } else { None };
                Ok(Self::JoinedGroup { group, wrong_password: r.bool()? })
            })(),
            channel::ADD_CATEGORY | channel::REMOVE_CATEGORY => Ok(Self::Ignored),
            _ => return None,
        };
        Some(packet)
    }
}

/// Plugin messages to the server: each is its channel and its data.
pub mod serverbound {
    use super::*;

    pub fn request_secret() -> (&'static str, Vec<u8>) {
        (channel::REQUEST_SECRET, Writer::new().i32(COMPATIBILITY_VERSION).take())
    }
    /// Voice chat on or off (`UpdateStatePacket`): the others see it.
    pub fn update_state(disabled: bool) -> (&'static str, Vec<u8>) {
        (channel::UPDATE_STATE, Writer::new().bool(disabled).take())
    }
    pub fn join_group(group: u128, password: Option<&str>) -> (&'static str, Vec<u8>) {
        let mut w = Writer::new();
        w.uuid(group).bool(password.is_some());
        if let Some(p) = password {
            w.utf(p);
        }
        (channel::SET_GROUP, w.take())
    }
    pub fn create_group(name: &str, password: Option<&str>, kind: GroupType) -> (&'static str, Vec<u8>) {
        let mut w = Writer::new();
        w.utf(name).bool(password.is_some());
        if let Some(p) = password {
            w.utf(p);
        }
        w.i16(kind.to_short());
        (channel::CREATE_GROUP, w.take())
    }
    pub fn leave_group() -> (&'static str, Vec<u8>) {
        (channel::LEAVE_GROUP, Vec::new())
    }
}

// UDP: every packet is the magic byte, then (to the server) the player's UUID, then the
// encrypted payload as a byte array: the packet's type and its fields.

pub const MAGIC: u8 = 0xFF;
/// `Utils.MAX_VOICE_CHAT_PACKET_SIZE`.
pub const MAX_PACKET: usize = 2048;
/// `AudioUtils.MAX_OPUS_PAYLOAD_SIZE`.
const MAX_OPUS: usize = 1024;

const MIC: u8 = 0x1;
const PLAYER_SOUND: u8 = 0x2;
const GROUP_SOUND: u8 = 0x3;
const LOCATION_SOUND: u8 = 0x4;
const AUTHENTICATE: u8 = 0x5;
const AUTHENTICATE_ACK: u8 = 0x6;
const PING: u8 = 0x7;
const KEEP_ALIVE: u8 = 0x8;
const CONNECTION_CHECK: u8 = 0x9;
const CONNECTION_CHECK_ACK: u8 = 0xA;

const HAS_CATEGORY: u8 = 0b10;
const WHISPER: u8 = 0b1;

/// Where a sound plays.
#[derive(Clone, Debug, PartialEq)]
pub enum SoundKind {
    /// From an entity (a player), heard up to `distance` blocks.
    Entity { whispering: bool, distance: f32 },
    /// From a group member: in the head, at any distance.
    Group,
    /// From a fixed place.
    Location { at: [f64; 3], distance: f32 },
}

/// A sound from the voice server.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    /// Its stream: one per speaker (and per add-on channel).
    pub channel: u128,
    /// The entity (player) it comes from.
    pub sender: u128,
    /// Opus; empty at the end of a stream.
    pub data: Vec<u8>,
    pub sequence: i64,
    pub kind: SoundKind,
    pub category: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UdpIn {
    Sound(Sound),
    AuthenticateAck,
    ConnectionCheckAck,
    /// A ping to send back as it is.
    Ping(Vec<u8>),
    KeepAlive,
    Other,
}

impl UdpIn {
    /// A datagram from the voice server, decrypted with `gcm`.
    pub fn decode(gcm: &Gcm, datagram: &[u8]) -> Result<Self> {
        let mut r = Reader::new(datagram);
        if r.u8()? != MAGIC {
            return Err(Malformed("not a voice packet"));
        }
        let payload = r.byte_array(MAX_PACKET)?;
        let plain = gcm.decrypt(payload).ok_or(Malformed("can't decrypt"))?;
        let mut r = Reader::new(&plain);
        let kind = r.u8()?;
        let r = &mut r;
        let sound = |r: &mut Reader, kind: u8| -> Result<Sound> {
            let channel = r.uuid()?;
            let sender = r.uuid()?;
            let at = if kind == LOCATION_SOUND { Some([r.f64()?, r.f64()?, r.f64()?]) } else { None };
            let data = r.byte_array(MAX_OPUS)?.to_vec();
            let sequence = r.i64()?;
            let distance = if kind == GROUP_SOUND { 0.0 } else { r.f32()? };
            let flags = r.u8()?;
            let category = if flags & HAS_CATEGORY != 0 { Some(r.utf(16)?) } else { None };
            let kind = match (kind, at) {
                (PLAYER_SOUND, _) => SoundKind::Entity { whispering: flags & WHISPER != 0, distance },
                (LOCATION_SOUND, Some(at)) => SoundKind::Location { at, distance },
                _ => SoundKind::Group,
            };
            Ok(Sound { channel, sender, data, sequence, kind, category })
        };
        Ok(match kind {
            PLAYER_SOUND | GROUP_SOUND | LOCATION_SOUND => Self::Sound(sound(r, kind)?),
            AUTHENTICATE_ACK => Self::AuthenticateAck,
            CONNECTION_CHECK_ACK => Self::ConnectionCheckAck,
            PING => Self::Ping(r.bytes(24)?.to_vec()),
            KEEP_ALIVE => Self::KeepAlive,
            _ => Self::Other,
        })
    }
}

/// A datagram to the voice server: `kind` and `fields`, encrypted.
fn udp(gcm: &Gcm, player: u128, kind: u8, fields: &[u8]) -> Option<Vec<u8>> {
    let mut plain = Vec::with_capacity(1 + fields.len());
    plain.push(kind);
    plain.extend_from_slice(fields);
    let sealed = gcm.encrypt(&plain)?;
    Some(Writer::new().u8(MAGIC).uuid(player).byte_array(&sealed).take())
}

pub fn udp_authenticate(gcm: &Gcm, player: u128, secret: &[u8; 16]) -> Option<Vec<u8>> {
    udp(gcm, player, AUTHENTICATE, &Writer::new().uuid(player).raw(secret).take())
}
pub fn udp_connection_check(gcm: &Gcm, player: u128) -> Option<Vec<u8>> {
    udp(gcm, player, CONNECTION_CHECK, &[])
}
pub fn udp_keep_alive(gcm: &Gcm, player: u128) -> Option<Vec<u8>> {
    udp(gcm, player, KEEP_ALIVE, &[])
}
/// The ping's own fields (its id and time), back.
pub fn udp_ping(gcm: &Gcm, player: u128, fields: &[u8]) -> Option<Vec<u8>> {
    udp(gcm, player, PING, fields)
}
/// `MicPacket`: one Opus frame (empty: the end of a stream).
pub fn udp_mic(gcm: &Gcm, player: u128, opus: &[u8], sequence: i64, whispering: bool) -> Option<Vec<u8>> {
    udp(gcm, player, MIC, &Writer::new().byte_array(opus).i64(sequence).bool(whispering).take())
}

/// Where the voice server is (`InitializationData.parseAddress`): `voice_host` may be empty,
/// a port, or a host with or without a port.
pub fn voice_address(voice_host: &str, server_host: &str, port: i32) -> (String, i32) {
    let host = voice_host.trim();
    if host.is_empty() {
        return (server_host.to_string(), port);
    }
    if let Ok(p) = host.parse::<i32>() {
        return (server_host.to_string(), if (1..=65535).contains(&p) { p } else { port });
    }
    let host = host.split_once("://").map_or(host, |(_, rest)| rest);
    let host = host.split('/').next().unwrap_or(host);
    if let Some(rest) = host.strip_prefix('[') {
        // [v6]:port
        let (h, after) = rest.split_once(']').unwrap_or((rest, ""));
        let p = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(port);
        return (h.to_string(), p);
    }
    match host.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().unwrap_or(port)),
        _ => (host.to_string(), port),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_secret() {
        let mut w = Writer::new();
        w.raw(&[9; 16]).i32(24454).uuid(42).u8(0).i32(1024).f64(48.0).i32(1000).bool(true).utf("").bool(false);
        let Some(Ok(Clientbound::Secret(s))) = Clientbound::decode(channel::SECRET, &w.data) else { panic!() };
        assert_eq!((s.port, s.player, s.codec, s.distance, s.keep_alive), (24454, 42, Codec::Voip, 48.0, 1000));
        assert!(s.groups_enabled);
    }

    #[test]
    fn reads_states_and_groups() {
        let mut w = Writer::new();
        w.i32(2);
        w.bool(false).bool(false).uuid(1).utf("Alex").bool(false);
        w.bool(true).bool(false).uuid(2).utf("Steve").bool(true).uuid(77);
        let Some(Ok(Clientbound::States(list))) = Clientbound::decode(channel::STATES, &w.data) else { panic!() };
        assert_eq!(list[1].name, "Steve");
        assert!(list[1].disabled);
        assert_eq!(list[1].group, Some(77));

        let mut w = Writer::new();
        w.uuid(77).utf("Team").bool(true).bool(false).bool(false).i16(2);
        let Some(Ok(Clientbound::AddGroup(g))) = Clientbound::decode(channel::ADD_GROUP, &w.data) else { panic!() };
        assert_eq!((g.name.as_str(), g.has_password, g.kind), ("Team", true, GroupType::Isolated));
        assert_eq!(Clientbound::decode("minecraft:brand", &[]), None);
    }

    /// A sound as the server writes it (`NetworkMessage.writeServer`).
    fn server_sound(gcm: &Gcm, fields: &[u8]) -> Vec<u8> {
        let sealed = gcm.encrypt(fields).unwrap();
        Writer::new().u8(MAGIC).byte_array(&sealed).take()
    }

    #[test]
    fn udp_sounds() {
        let gcm = Gcm::new(&[3; 16]);
        let mut w = Writer::new();
        w.u8(PLAYER_SOUND).uuid(5).uuid(6).byte_array(&[1, 2, 3]).i64(7).f32(15.0).u8(WHISPER | HAS_CATEGORY).utf("music");
        let Ok(UdpIn::Sound(s)) = UdpIn::decode(&gcm, &server_sound(&gcm, &w.data)) else { panic!() };
        assert_eq!((s.channel, s.sender, s.sequence), (5, 6, 7));
        assert_eq!(s.kind, SoundKind::Entity { whispering: true, distance: 15.0 });
        assert_eq!(s.category.as_deref(), Some("music"));

        let mut w = Writer::new();
        w.u8(LOCATION_SOUND).uuid(5).uuid(6).f64(1.0).f64(2.0).f64(3.0).byte_array(&[]).i64(8).f32(20.0).u8(0);
        let Ok(UdpIn::Sound(s)) = UdpIn::decode(&gcm, &server_sound(&gcm, &w.data)) else { panic!() };
        assert_eq!(s.kind, SoundKind::Location { at: [1.0, 2.0, 3.0], distance: 20.0 });

        let mut w = Writer::new();
        w.u8(GROUP_SOUND).uuid(5).uuid(6).byte_array(&[4]).i64(9).u8(0);
        let Ok(UdpIn::Sound(s)) = UdpIn::decode(&gcm, &server_sound(&gcm, &w.data)) else { panic!() };
        assert_eq!(s.kind, SoundKind::Group);
    }

    #[test]
    fn mic_packet_layout() {
        let gcm = Gcm::new(&[3; 16]);
        let d = udp_mic(&gcm, 0xABCD, &[1, 2], 5, true).unwrap();
        let mut r = Reader::new(&d);
        assert_eq!(r.u8().unwrap(), MAGIC);
        assert_eq!(r.uuid().unwrap(), 0xABCD);
        let plain = gcm.decrypt(r.byte_array(MAX_PACKET).unwrap()).unwrap();
        assert_eq!(plain, [MIC, 2, 1, 2, 0, 0, 0, 0, 0, 0, 0, 5, 1]);
    }

    #[test]
    fn voice_hosts() {
        assert_eq!(voice_address("", "mc.example.org", 24454), ("mc.example.org".into(), 24454));
        assert_eq!(voice_address("25000", "mc.example.org", 24454), ("mc.example.org".into(), 25000));
        assert_eq!(voice_address("voice.example.org", "mc", 24454), ("voice.example.org".into(), 24454));
        assert_eq!(voice_address("voice.example.org:1234", "mc", 24454), ("voice.example.org".into(), 1234));
        assert_eq!(voice_address("[::1]:1234", "mc", 24454), ("::1".into(), 1234));
    }
}
