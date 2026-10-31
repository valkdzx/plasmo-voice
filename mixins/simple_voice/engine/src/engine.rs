//! The client: what Simple Voice Chat's ClientManager, ClientVoicechatConnection, MicThread
//! and its audio channels do, for one game thread that hands it the plugin messages, the
//! listener and where players are.
//!
//! Threads: the game's (all of `Engine`), the UDP reader (authentication, keep-alives,
//! decrypting and decoding sounds into the mixer), the encoder (the microphone's frames out as
//! voice packets) and the two cpal streams.
use crate::audio::{self, Stream};
use crate::codec::{self, Decoder, Encoder};
use crate::dsp::{self, NoiseGate};
use crate::gcm::Gcm;
use crate::mixer::{Mixer, Resampler};
use crate::proto::{self, Clientbound, Group, GroupType, PlayerState, Secret, SoundKind, UdpIn, serverbound};
use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Voice activation keeps sending this many frames after the level drops (0.5 s).
const HANGOVER_FRAMES: u32 = 25;
/// The mixer's source for "hear yourself".
const MONITOR: u128 = u128::MAX;
/// No secret this long after asking, and the client asks again (a few times).
const SECRET_RETRY: Duration = Duration::from_secs(5);
const SECRET_TRIES: u32 = 3;

/// How the microphone starts sending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// While the talk key is held.
    #[default]
    PushToTalk,
    /// While the microphone is louder than the threshold.
    Voice,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::PushToTalk => "ptt",
            Self::Voice => "voice",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ptt" => Some(Self::PushToTalk),
            "voice" => Some(Self::Voice),
            _ => None,
        }
    }
}

/// What the player chose.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Voice chat off: hear nobody, send nothing, and the others see it.
    pub disabled: bool,
    pub muted: bool,
    pub mode: Mode,
    /// Voice activation's level, in dBFS.
    pub threshold_db: f32,
    /// 0 to 2.
    pub volume: f32,
    pub microphone_gain: f32,
    /// Device names; None is the system's default.
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// By player UUID, 0 to 2.
    pub player_volumes: HashMap<u128, f32>,
    /// Players this client doesn't hear.
    pub muted_players: HashSet<u128>,
    /// A noise gate: the room's hum between words stays home.
    pub noise_suppression: bool,
    /// Hear your own microphone (a test; not kept).
    pub monitor: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            disabled: false,
            muted: false,
            mode: Mode::PushToTalk,
            threshold_db: -50.0,
            volume: 1.0,
            microphone_gain: 1.0,
            input_device: None,
            output_device: None,
            player_volumes: HashMap::new(),
            muted_players: HashSet::new(),
            noise_suppression: false,
            monitor: false,
        }
    }
}

/// The player's ears: where they are and which way they face (Minecraft's yaw: 0 faces
/// south, +z; 90 west, -x).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    pub position: [f64; 3],
    pub yaw: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    /// No Simple Voice Chat on this server (yet).
    Off,
    /// Told where the voice server is; authenticating.
    Connecting,
    Connected,
    /// It stopped answering.
    TimedOut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub connection: Connection,
    pub talking: bool,
    pub whispering: bool,
    /// The microphone's last frame, in dBFS (after the gain).
    pub microphone_db: f32,
    /// Who is heard right now: name, and the player's UUID.
    pub speaking: Vec<(String, u128)>,
    pub microphone: Option<String>,
    pub speakers: Option<String>,
}

/// One stream heard: who it comes from and how it is placed.
#[derive(Clone, Debug)]
struct Heard {
    sender: u128,
    kind: SoundKind,
}

/// One connection to a voice server, shared by its threads.
struct Link {
    socket: UdpSocket,
    gcm: Gcm,
    secret: [u8; 16],
    player: u128,
    codec: proto::Codec,
    keep_alive_ms: i64,
    stop: AtomicBool,
    authenticated: AtomicBool,
    connected: AtomicBool,
    /// The last keep-alive from the server, in ms since the epoch.
    keepalive: AtomicI64,
    settings: Mutex<Arc<Settings>>,
    /// The talk and whisper keys.
    talk: AtomicBool,
    whisper: AtomicBool,
    heard: Mutex<HashMap<u128, Heard>>,
    decoders: Mutex<HashMap<u128, (Decoder, i64)>>,
    mixer: Arc<Mutex<Mixer>>,
    errors: Mutex<Vec<String>>,
    /// 0 silent, 1 talking, 2 whispering.
    sending: AtomicU32,
    microphone_db: AtomicU32,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl Link {
    fn connection(&self) -> Connection {
        if !(self.authenticated.load(Ordering::Relaxed) && self.connected.load(Ordering::Relaxed)) {
            return Connection::Connecting;
        }
        // Simple Voice Chat drops the link after ten missed keep-alives.
        if now_ms() - self.keepalive.load(Ordering::Relaxed) > self.keep_alive_ms * 10 {
            return Connection::TimedOut;
        }
        Connection::Connected
    }
    fn send(&self, datagram: Option<Vec<u8>>) {
        if let Some(d) = datagram {
            let _ = self.socket.send(&d);
        }
    }
    fn error(&self, e: String) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.push(e);
        }
    }
    fn settings(&self) -> Arc<Settings> {
        self.settings.lock().unwrap().clone()
    }
}

/// The microphone, the speakers and the encoder of a connected link.
struct Devices {
    capture: Option<Stream>,
    playback: Option<Stream>,
    encoder: Option<JoinHandle<()>>,
}

pub struct Engine {
    settings: Settings,
    talk: bool,
    whisper: bool,
    /// The game server's host, for a voice server at the same address.
    server_host: String,
    /// When the secret was last asked for, and how many times.
    asked: Option<(Instant, u32)>,
    secret: Option<Secret>,
    link: Option<Arc<Link>>,
    net: Option<JoinHandle<()>>,
    devices: Option<Devices>,
    /// The voice chat's state was told to the server on this link.
    state_sent: bool,
    players: HashMap<u128, PlayerState>,
    groups: HashMap<u128, Group>,
    group: Option<u128>,
    outgoing: Vec<(String, Vec<u8>)>,
    messages: Vec<String>,
    /// How each stream heard was placed at the last tick (`/vc debug`).
    placed: Vec<String>,
    ticks: u64,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

impl Engine {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            talk: false,
            whisper: false,
            server_host: String::new(),
            asked: None,
            secret: None,
            link: None,
            net: None,
            devices: None,
            state_sent: false,
            players: HashMap::new(),
            groups: HashMap::new(),
            group: None,
            outgoing: Vec::new(),
            messages: Vec::new(),
            placed: Vec::new(),
            ticks: 0,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// New settings: they apply at once, devices reopen when theirs changed, and the server
    /// hears of voice chat going off or on.
    pub fn set_settings(&mut self, settings: Settings) {
        let old = std::mem::replace(&mut self.settings, settings.clone());
        if let Some(link) = &self.link {
            *link.settings.lock().unwrap() = Arc::new(settings.clone());
            if let Ok(mut m) = link.mixer.lock() {
                m.master = settings.volume;
                if settings.disabled {
                    m.clear();
                }
            }
        }
        if old.disabled != settings.disabled && self.secret.is_some() {
            self.outgoing.push(owned(serverbound::update_state(settings.disabled)));
        }
        if (old.input_device, old.output_device) != (settings.input_device.clone(), settings.output_device.clone())
            && self.devices.is_some()
        {
            self.devices = None;
            self.start_devices();
        }
    }

    /// The talk key went down or up.
    pub fn set_talk(&mut self, down: bool) {
        self.talk = down;
        if let Some(link) = &self.link {
            link.talk.store(down, Ordering::Relaxed);
        }
    }
    /// The whisper key went down or up: it talks too, quieter and nearer.
    pub fn set_whisper(&mut self, down: bool) {
        self.whisper = down;
        if let Some(link) = &self.link {
            link.whisper.store(down, Ordering::Relaxed);
        }
    }
    /// Every key up: the player opened a screen.
    pub fn release_keys(&mut self) {
        self.set_talk(false);
        self.set_whisper(false);
    }

    /// The game joined a world: ask the server for the voice chat's secret.
    pub fn joined(&mut self) {
        self.close_link();
        self.secret = None;
        self.players.clear();
        self.groups.clear();
        self.group = None;
        self.outgoing.push(owned(serverbound::request_secret()));
        self.asked = Some((Instant::now(), 1));
    }

    /// A plugin message from the server; false when it isn't Simple Voice Chat's.
    pub fn receive(&mut self, channel: &str, data: &[u8]) -> bool {
        let packet = match Clientbound::decode(channel, data) {
            None => return false,
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                eprintln!("Simple Voice: bad packet on {channel}: {}", e.0);
                return true;
            }
        };
        match packet {
            Clientbound::Secret(s) => self.connect(*s),
            Clientbound::State(p) => {
                self.players.insert(p.id, p);
            }
            Clientbound::States(list) => {
                self.players = list.into_iter().map(|p| (p.id, p)).collect();
            }
            Clientbound::RemoveState(id) => {
                self.players.remove(&id);
            }
            Clientbound::AddGroup(g) => {
                self.groups.insert(g.id, g);
            }
            Clientbound::RemoveGroup(id) => {
                self.groups.remove(&id);
            }
            Clientbound::JoinedGroup { group, wrong_password } => {
                if wrong_password {
                    self.messages.push("Wrong group password".into());
                }
                self.group = group;
            }
            Clientbound::Ignored => {}
        }
        true
    }

    /// `SecretPacket`: a fresh UDP link that authenticates with the voice server.
    fn connect(&mut self, secret: Secret) {
        self.close_link();
        self.asked = None;
        let (host, port) = proto::voice_address(&secret.voice_host, &self.server_host, secret.port);
        self.secret = Some(secret.clone());
        let Ok(port) = u16::try_from(port) else { return };
        let address = match (host.as_str(), port).to_socket_addrs().map(|mut a| a.next()) {
            Ok(Some(a)) => a,
            _ => {
                self.messages.push(format!("Voice chat: can't find the voice server {host}:{port}"));
                return;
            }
        };
        let bind: SocketAddr = if address.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
        let socket = match UdpSocket::bind(bind).and_then(|s| s.connect(address).map(|_| s)) {
            Ok(s) => s,
            Err(e) => {
                self.messages.push(format!("Voice chat: can't reach the voice server: {e}"));
                return;
            }
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
        let mut mixer = Mixer::new(codec::RATE as u32);
        mixer.master = self.settings.volume;
        let link = Arc::new(Link {
            socket,
            gcm: Gcm::new(&secret.secret),
            secret: secret.secret,
            player: secret.player,
            codec: secret.codec,
            keep_alive_ms: secret.keep_alive.max(100) as i64,
            stop: AtomicBool::new(false),
            authenticated: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            keepalive: AtomicI64::new(now_ms()),
            settings: Mutex::new(Arc::new(self.settings.clone())),
            talk: AtomicBool::new(self.talk),
            whisper: AtomicBool::new(self.whisper),
            heard: Mutex::new(HashMap::new()),
            decoders: Mutex::new(HashMap::new()),
            mixer: Arc::new(Mutex::new(mixer)),
            errors: Mutex::new(Vec::new()),
            sending: AtomicU32::new(0),
            microphone_db: AtomicU32::new((-100f32).to_bits()),
        });
        let reader = link.clone();
        self.net = std::thread::Builder::new()
            .name("simple-voice-udp".into())
            .spawn(move || udp_loop(reader))
            .ok();
        self.link = Some(link);
        self.state_sent = false;
        self.start_devices();
    }

    fn start_devices(&mut self) {
        let Some(link) = self.link.clone() else { return };
        let mut devices = Devices { capture: None, playback: None, encoder: None };
        match audio::playback(self.settings.output_device.clone(), link.mixer.clone()) {
            Ok(s) => devices.playback = Some(s),
            Err(e) => self.messages.push(format!("Voice chat: can't open the speakers: {e}")),
        }
        let (sink, blocks) = mpsc::sync_channel(64);
        match audio::capture(self.settings.input_device.clone(), sink) {
            Ok(s) => {
                let (rate, encoder_link) = (s.rate, link.clone());
                devices.encoder = std::thread::Builder::new()
                    .name("simple-voice-encoder".into())
                    .spawn(move || encode_loop(encoder_link, blocks, rate))
                    .ok();
                devices.capture = Some(s);
            }
            Err(e) => self.messages.push(format!("Voice chat: can't open the microphone: {e}")),
        }
        self.devices = Some(devices);
    }

    fn close_link(&mut self) {
        // The capture stream goes first: its end closes the encoder's channel.
        self.devices = None;
        if let Some(link) = self.link.take() {
            link.stop.store(true, Ordering::Relaxed);
        }
        // The UDP reader sees `stop` within 100 ms; not joined, as the streams aren't.
        self.net.take();
    }

    /// The game left the server: everything goes.
    pub fn reset(&mut self) {
        self.close_link();
        self.secret = None;
        self.asked = None;
        self.players.clear();
        self.groups.clear();
        self.group = None;
        self.outgoing.clear();
    }

    /// Once a tick: `server_host` is the game server's host, `listener` the player (None out of
    /// a world), `locate` finds a player (or another entity) by UUID.
    pub fn tick(
        &mut self,
        server_host: &str,
        listener: Option<Listener>,
        locate: &mut dyn FnMut(u128) -> Option<[f64; 3]>,
    ) {
        if self.server_host != server_host {
            self.server_host = server_host.to_string();
        }
        self.ticks += 1;
        // No answer to the secret request: ask again, a few times.
        if let Some((at, tries)) = self.asked
            && at.elapsed() > SECRET_RETRY
        {
            if tries < SECRET_TRIES {
                self.outgoing.push(owned(serverbound::request_secret()));
                self.asked = Some((Instant::now(), tries + 1));
            } else {
                self.asked = None;
            }
        }
        let Some(link) = self.link.clone() else { return };
        self.messages.append(&mut link.errors.lock().unwrap());
        match link.connection() {
            Connection::Connected if !self.state_sent => {
                self.outgoing.push(owned(serverbound::update_state(self.settings.disabled)));
                self.state_sent = true;
            }
            Connection::TimedOut => {
                // Gone: start over, as Simple Voice Chat reconnects.
                self.messages.push("Voice chat: connection lost, reconnecting".into());
                self.close_link();
                self.outgoing.push(owned(serverbound::request_secret()));
                self.asked = Some((Instant::now(), 1));
                return;
            }
            _ => {}
        }
        // Place what is heard.
        let s = &self.settings;
        let live = link.mixer.lock().unwrap().heard(Duration::from_secs(2));
        let heard = link.heard.lock().unwrap().clone();
        let mut gains = Vec::with_capacity(live.len());
        let mut placed = Vec::new();
        for (channel, _) in live {
            if channel == MONITOR {
                gains.push((channel, [CENTRE, CENTRE]));
                continue;
            }
            let Some(h) = heard.get(&channel) else { continue };
            let mut volume = s.player_volumes.get(&h.sender).copied().unwrap_or(1.0);
            if s.muted_players.contains(&h.sender) {
                volume = 0.0;
            }
            let (at, distance) = match &h.kind {
                SoundKind::Entity { distance, .. } => (locate(h.sender), *distance as f64),
                SoundKind::Location { at, distance } => (Some(*at), *distance as f64),
                SoundKind::Group => (None, 0.0),
            };
            let [l, r] = spatial(listener, at, distance);
            let name = self.players.get(&h.sender).map_or_else(|| format!("{:032x}", h.sender), |p| p.name.clone());
            let away = match (listener, at, &h.kind) {
                (_, _, SoundKind::Group) => "group".to_string(),
                (Some(l), Some(a), _) => format!(
                    "{:.1} blocks away",
                    ((a[0] - l.position[0]).powi(2) + (a[1] - l.position[1]).powi(2) + (a[2] - l.position[2]).powi(2)).sqrt()
                ),
                (None, _, _) => "no listener".into(),
                (_, None, _) => "NOT FOUND in the world".into(),
            };
            placed.push(format!("{name}: {away}, carries {distance:.0}, volume {volume:.2}, left {l:.2} right {r:.2}"));
            gains.push((channel, [l * volume, r * volume]));
        }
        self.placed = placed;
        let mut mixer = link.mixer.lock().unwrap();
        for (id, g) in gains {
            mixer.set_gains(id, g);
        }
    }

    pub fn join_group(&mut self, group: u128, password: Option<&str>) {
        self.outgoing.push(owned(serverbound::join_group(group, password)));
    }
    pub fn create_group(&mut self, name: &str, password: Option<&str>, kind: GroupType) {
        self.outgoing.push(owned(serverbound::create_group(name, password, kind)));
    }
    pub fn leave_group(&mut self) {
        self.outgoing.push(owned(serverbound::leave_group()));
    }

    /// Plugin messages to send: channel and data.
    pub fn take_outgoing(&mut self) -> Vec<(String, Vec<u8>)> {
        std::mem::take(&mut self.outgoing)
    }
    /// Lines for the player's chat.
    pub fn take_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.messages)
    }

    /// How each stream heard at the last tick was placed, and how many ticks ran.
    pub fn debug(&self) -> (u64, Vec<String>) {
        (self.ticks, self.placed.clone())
    }

    /// The players the server told about, by name.
    pub fn players(&self) -> Vec<PlayerState> {
        let mut list: Vec<PlayerState> = self.players.values().cloned().collect();
        list.sort_by_key(|p| p.name.to_lowercase());
        list
    }
    pub fn player(&self, id: u128) -> Option<&PlayerState> {
        self.players.get(&id)
    }
    /// This player's UUID, once the secret came.
    pub fn me(&self) -> Option<u128> {
        self.secret.as_ref().map(|s| s.player)
    }
    pub fn groups_enabled(&self) -> bool {
        self.secret.as_ref().is_some_and(|s| s.groups_enabled)
    }
    /// The groups the server lists, by name (hidden ones only when this player is in them).
    pub fn groups(&self) -> Vec<Group> {
        let mut list: Vec<Group> = self
            .groups
            .values()
            .filter(|g| !g.hidden || Some(g.id) == self.group)
            .cloned()
            .collect();
        list.sort_by_key(|g| g.name.to_lowercase());
        list
    }
    pub fn group(&self) -> Option<&Group> {
        self.groups.get(&self.group?)
    }
    /// The members of group `id`, by name.
    pub fn members(&self, id: u128) -> Vec<String> {
        let mut names: Vec<String> = self.players.values().filter(|p| p.group == Some(id)).map(|p| p.name.clone()).collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    pub fn status(&self) -> Status {
        let Some(link) = &self.link else {
            return Status {
                connection: Connection::Off,
                talking: false,
                whispering: false,
                microphone_db: -100.0,
                speaking: Vec::new(),
                microphone: None,
                speakers: None,
            };
        };
        let heard = link.heard.lock().unwrap();
        let mut speaking: Vec<(String, u128)> = link
            .mixer
            .lock()
            .unwrap()
            .heard(Duration::from_millis(300))
            .into_iter()
            .filter_map(|(id, _)| heard.get(&id))
            .filter_map(|h| self.players.get(&h.sender).map(|p| (p.name.clone(), p.id)))
            .collect();
        speaking.sort();
        speaking.dedup();
        let sending = link.sending.load(Ordering::Relaxed);
        Status {
            connection: link.connection(),
            talking: sending > 0,
            whispering: sending == 2,
            microphone_db: f32::from_bits(link.microphone_db.load(Ordering::Relaxed)),
            speaking,
            microphone: self.devices.as_ref().and_then(|d| d.capture.as_ref()).map(|s| s.name.clone()),
            speakers: self.devices.as_ref().and_then(|d| d.playback.as_ref()).map(|s| s.name.clone()),
        }
    }

    pub fn input_devices() -> Vec<String> {
        audio::input_devices()
    }
    pub fn output_devices() -> Vec<String> {
        audio::output_devices()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.close_link();
    }
}

fn owned((channel, data): (&str, Vec<u8>)) -> (String, Vec<u8>) {
    (channel.to_string(), data)
}

/// The gain of a mono source in front in each ear: what OpenAL gives a centred source.
pub const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Left and right gains of a source at `at` heard by `listener`: linear falloff to
/// `max_distance` (Simple Voice Chat's `getDistanceVolume`) and equal-power panning by the
/// side it is on, as OpenAL places its sources. A source with no place (a group) plays in the
/// middle.
pub fn spatial(listener: Option<Listener>, at: Option<[f64; 3]>, max_distance: f64) -> [f32; 2] {
    let (Some(l), Some(at)) = (listener, at) else { return [CENTRE, CENTRE] };
    let d = [at[0] - l.position[0], at[1] - l.position[1], at[2] - l.position[2]];
    let distance = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let falloff = if max_distance > 0.0 { 1.0 - distance.min(max_distance) / max_distance } else { 1.0 };
    let horizontal = (d[0] * d[0] + d[2] * d[2]).sqrt();
    let pan = if horizontal < 0.5 {
        0.0
    } else {
        // The listener's right: west when facing south.
        let yaw = (l.yaw as f64).to_radians();
        let right = [-yaw.cos(), -yaw.sin()];
        ((d[0] * right[0] + d[2] * right[1]) / horizontal).clamp(-1.0, 1.0)
    };
    let left = ((1.0 - pan) / 2.0).sqrt() * falloff;
    let right = ((1.0 + pan) / 2.0).sqrt() * falloff;
    [left as f32, right as f32]
}

/// The UDP reader: authenticates and checks the connection once a second until the server
/// acknowledges both, answers keep-alives and pings, and decodes sounds into the mixer.
fn udp_loop(link: Arc<Link>) {
    let mut buf = vec![0u8; 4096];
    let mut last_try: Option<Instant> = None;
    while !link.stop.load(Ordering::Relaxed) {
        let authenticated = link.authenticated.load(Ordering::Relaxed);
        let connected = link.connected.load(Ordering::Relaxed);
        if !(authenticated && connected) && last_try.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            last_try = Some(Instant::now());
            if !authenticated {
                link.send(proto::udp_authenticate(&link.gcm, link.player, &link.secret));
            } else {
                link.send(proto::udp_connection_check(&link.gcm, link.player));
            }
        }
        // Errors are timeouts, or (Windows) the ICMP of a port that isn't open yet.
        let Ok(n) = link.socket.recv(&mut buf) else { continue };
        match UdpIn::decode(&link.gcm, &buf[..n]) {
            Ok(UdpIn::AuthenticateAck) => {
                link.authenticated.store(true, Ordering::Relaxed);
                // Straight on to the connection check.
                last_try = None;
            }
            Ok(UdpIn::ConnectionCheckAck) => {
                if link.authenticated.load(Ordering::Relaxed) {
                    link.keepalive.store(now_ms(), Ordering::Relaxed);
                    link.connected.store(true, Ordering::Relaxed);
                }
            }
            Ok(UdpIn::KeepAlive) => {
                link.keepalive.store(now_ms(), Ordering::Relaxed);
                link.send(proto::udp_keep_alive(&link.gcm, link.player));
            }
            Ok(UdpIn::Ping(fields)) => link.send(proto::udp_ping(&link.gcm, link.player, &fields)),
            Ok(UdpIn::Sound(sound)) => hear(&link, sound),
            Ok(UdpIn::Other) | Err(_) => {}
        }
    }
}

fn hear(link: &Link, sound: proto::Sound) {
    if link.settings().disabled {
        return;
    }
    let channel = sound.channel;
    if sound.data.is_empty() {
        // The end of a stream: play out what is left.
        link.mixer.lock().unwrap().end(channel);
        link.decoders.lock().unwrap().remove(&channel);
        return;
    }
    link.heard.lock().unwrap().insert(channel, Heard { sender: sound.sender, kind: sound.kind.clone() });
    let mut decoders = link.decoders.lock().unwrap();
    let (decoder, last) = match decoders.entry(channel) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => match Decoder::new() {
            Ok(d) => e.insert((d, sound.sequence - 1)),
            Err(err) => {
                link.error(format!("Voice chat: {err}"));
                return;
            }
        },
    };
    if sound.sequence <= *last {
        // Late or repeated.
        return;
    }
    let mut pcm = Vec::new();
    // A few lost packets are concealed; a longer gap just restarts.
    if (2..=4).contains(&(sound.sequence - *last)) {
        for _ in *last + 1..sound.sequence {
            if let Some(p) = decoder.decode(&[]) {
                pcm.extend(p);
            }
        }
    }
    *last = sound.sequence;
    if let Some(p) = decoder.decode(&sound.data) {
        pcm.extend(p);
    }
    drop(decoders);
    let distance = match sound.kind {
        SoundKind::Entity { distance, .. } | SoundKind::Location { distance, .. } => distance as i16,
        SoundKind::Group => 0,
    };
    link.mixer.lock().unwrap().push(channel, 1, &pcm, distance);
}

/// The encoder: the microphone's blocks, resampled to 48 kHz, cut into 20 ms frames and
/// filtered, go out as voice packets while the player talks; an empty packet ends each stream.
fn encode_loop(link: Arc<Link>, blocks: mpsc::Receiver<Vec<f32>>, device_rate: u32) {
    let mut resampler = Resampler::new(device_rate, codec::RATE as u32);
    let mut pending: Vec<f32> = Vec::new();
    let mut encoder = match Encoder::new(link.codec) {
        Ok(e) => e,
        Err(e) => {
            link.error(format!("Voice chat: {e}"));
            return;
        }
    };
    let mut sequence: i64 = 0;
    let mut active = false;
    let mut hangover = 0u32;
    let mut gate = NoiseGate::default();
    loop {
        let block = match blocks.recv_timeout(Duration::from_millis(200)) {
            Ok(b) => b,
            Err(RecvTimeoutError::Timeout) if !link.stop.load(Ordering::Relaxed) => continue,
            _ => break,
        };
        resampler.process(&block, &mut pending);
        while pending.len() >= codec::FRAME {
            let s = link.settings();
            let mut frame: Vec<f32> = pending.drain(..codec::FRAME).map(|x| x * s.microphone_gain).collect();
            if s.noise_suppression {
                // Under the activation level (or a quiet room's, for push to talk) is noise.
                gate.process(&mut frame, s.threshold_db.min(-40.0));
            }
            dsp::limit(&mut frame);
            for x in frame.iter_mut() {
                *x = x.clamp(-1.0, 1.0);
            }
            let db = dsp::level_db(&frame);
            link.microphone_db.store(db.to_bits(), Ordering::Relaxed);
            if s.monitor {
                link.mixer.lock().unwrap().push(MONITOR, 1, &frame, 0);
            }
            hangover = if db > s.threshold_db { HANGOVER_FRAMES } else { hangover.saturating_sub(1) };
            let whisper = link.whisper.load(Ordering::Relaxed);
            let keys = link.talk.load(Ordering::Relaxed) || whisper;
            let wants = !s.muted
                && !s.disabled
                && link.connection() == Connection::Connected
                && match s.mode {
                    Mode::PushToTalk => keys,
                    Mode::Voice => hangover > 0,
                };
            if wants {
                let mut f = frame;
                if !active {
                    // A stream starts from silence: no click.
                    f.iter_mut().take(48).enumerate().for_each(|(i, x)| *x *= i as f32 / 48.0);
                }
                if let Some(packet) = encoder.encode(&f) {
                    link.send(proto::udp_mic(&link.gcm, link.player, &packet, sequence, whisper));
                    sequence += 1;
                    active = true;
                }
            } else if active {
                link.send(proto::udp_mic(&link.gcm, link.player, &[], sequence, false));
                sequence += 1;
                encoder.reset();
                active = false;
            }
            let state = match (active, whisper) {
                (false, _) => 0,
                (true, false) => 1,
                (true, true) => 2,
            };
            link.sending.store(state, Ordering::Relaxed);
        }
    }
    link.sending.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buf::Writer;

    #[test]
    fn panning_follows_the_listener() {
        // Facing south (+z), a source to the west (-x) is on the right.
        let l = Listener { position: [0.0, 0.0, 0.0], yaw: 0.0 };
        let [left, right] = spatial(Some(l), Some([-5.0, 0.0, 0.0]), 10.0);
        assert!(right > left);
        // Halfway: half the volume, in power.
        assert!(((left * left + right * right).sqrt() - 0.5).abs() < 1e-4);
        assert_eq!(spatial(Some(l), Some([20.0, 0.0, 0.0]), 10.0), [0.0, 0.0]);
        assert_eq!(spatial(Some(l), None, 10.0), [CENTRE, CENTRE]);
    }

    #[test]
    fn asks_for_the_secret_and_follows_states() {
        let mut e = Engine::default();
        e.joined();
        let out = e.take_outgoing();
        assert_eq!(out[0].0, proto::channel::REQUEST_SECRET);
        assert_eq!(out[0].1, proto::COMPATIBILITY_VERSION.to_be_bytes());

        let mut w = Writer::new();
        w.bool(false).bool(false).uuid(9).utf("Alex").bool(true).uuid(4);
        assert!(e.receive(proto::channel::STATE, &w.data));
        let mut w = Writer::new();
        w.uuid(4).utf("Team").bool(false).bool(false).bool(false).i16(0);
        assert!(e.receive(proto::channel::ADD_GROUP, &w.data));
        assert_eq!(e.members(4), ["Alex"]);
        assert!(e.receive(proto::channel::JOINED_GROUP, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0]));
        assert_eq!(e.group().map(|g| g.name.as_str()), Some("Team"));
        assert!(!e.receive("minecraft:brand", b"vanilla"));
    }

    /// A whole session against a voice server played here: the secret, authentication, the
    /// connection check, a keep-alive and a sound.
    #[test]
    fn talks_to_a_voice_server() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let port = server.local_addr().unwrap().port();
        let key = [5u8; 16];
        let gcm = Gcm::new(&key);
        let mut w = Writer::new();
        w.raw(&key).i32(port as i32).uuid(77).u8(0).i32(1024).f64(48.0).i32(1000).bool(true).utf("127.0.0.1").bool(false);
        let mut e = Engine::default();
        assert!(e.receive(proto::channel::SECRET, &w.data));
        let mut buf = [0u8; 4096];
        let mut read = |server: &UdpSocket| -> (Vec<u8>, SocketAddr) {
            let (n, from) = server.recv_from(&mut buf).unwrap();
            let mut r = crate::buf::Reader::new(&buf[..n]);
            assert_eq!(r.u8().unwrap(), proto::MAGIC);
            assert_eq!(r.uuid().unwrap(), 77);
            (gcm.decrypt(r.byte_array(proto::MAX_PACKET).unwrap()).unwrap(), from)
        };
        let reply = |server: &UdpSocket, to: SocketAddr, fields: &[u8]| {
            let sealed = gcm.encrypt(fields).unwrap();
            server.send_to(&Writer::new().u8(proto::MAGIC).byte_array(&sealed).take(), to).unwrap();
        };
        // Authenticate: its UUID and the secret.
        let (plain, client) = read(&server);
        assert_eq!(plain[0], 0x5);
        assert_eq!(&plain[17..], &key);
        reply(&server, client, &[0x6]);
        // The connection check.
        let (plain, _) = read(&server);
        assert_eq!(plain, [0x9]);
        reply(&server, client, &[0xA]);
        // A keep-alive comes back.
        reply(&server, client, &[0x8]);
        let (plain, _) = read(&server);
        assert_eq!(plain, [0x8]);
        assert_eq!(e.status().connection, Connection::Connected);
        // A player's sound reaches the mixer.
        let mut opus = Encoder::new(proto::Codec::Voip).unwrap();
        let packet = opus.encode(&vec![0.1; codec::FRAME]).unwrap();
        let mut w = Writer::new();
        w.u8(0x2).uuid(1).uuid(2).byte_array(&packet).i64(0).f32(48.0).u8(0);
        reply(&server, client, &w.data);
        let start = Instant::now();
        while e.link.as_ref().unwrap().heard.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(e.link.as_ref().unwrap().heard.lock().unwrap().get(&1).map(|h| h.sender), Some(2));
        e.tick("127.0.0.1", None, &mut |_| None);
        // Connected: the voice chat's state goes to the server.
        assert!(e.take_outgoing().iter().any(|(c, d)| c == proto::channel::UPDATE_STATE && d == &[0]));
    }
}
