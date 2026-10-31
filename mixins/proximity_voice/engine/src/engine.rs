//! The client: what Plasmo Voice's ModServerConnection, NettyUdpClient, VoiceAudioCapture,
//! ClientActivationManager and its audio sources do, for one game thread that hands it the
//! channel's payloads, the listener and where sources are.
//!
//! Threads: the game's (all of `Engine`), the UDP reader (pings, decrypting and decoding
//! sources into the mixer), the encoder (the microphone's frames out as voice packets of each
//! activation that speaks), and the two cpal streams.
use crate::aes::{Aes128, cbc_decrypt, cbc_encrypt};
use crate::audio::{self, Stream};
use crate::codec::{Decoder, Encoder};
use crate::dsp::{self, Compressor, NoiseGate};
use crate::mixer::{Mixer, Resampler};
use crate::proto::{self, Activation, Clientbound, CodecInfo, Config, SourceInfo, SourceKind, UdpIn, VoicePlayer, serverbound};
use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The Plasmo Voice version this client speaks (`PlayerInfoPacket.version`): the server
/// wants its own major version and at least its `client_mod_min_version` (2.0.0 by default).
pub const PROTOCOL_VERSION: &str = "2.2.0";
/// No ping from the voice server for this long and the connection shows as timed out
/// (`MAX_SOFT_KEEP_ALIVE_TIMEOUT`); for `LOST_MS`, it is gone and the client pings again.
const SOFT_TIMEOUT_MS: i64 = 7_000;
const LOST_MS: i64 = 30_000;
/// Voice activation keeps sending this many frames after the level drops (0.5 s).
const HANGOVER_FRAMES: u32 = 25;
/// The mixer's source for "hear yourself".
const MONITOR: u128 = u128::MAX;

/// This client's RSA key pair: the server encrypts the voice key with its public half.
pub trait RsaKey: Send + Sync {
    /// The public key as X.509 SubjectPublicKeyInfo DER (`PublicKey.getEncoded`).
    fn public_der(&self) -> Vec<u8>;
    /// RSA/ECB/PKCS1Padding decryption.
    fn decrypt(&self, data: &[u8]) -> Option<Vec<u8>>;
}

/// How an activation starts speaking (`ClientActivation.Type`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// While its key is held (or, with `toggle`, between two presses).
    #[default]
    PushToTalk,
    /// While the microphone is louder than the threshold.
    Voice,
    /// As proximity does (the default of every other activation).
    Inherit,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::PushToTalk => "ptt",
            Self::Voice => "voice",
            Self::Inherit => "inherit",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ptt" => Self::PushToTalk,
            "voice" => Self::Voice,
            "inherit" => Self::Inherit,
            _ => return None,
        })
    }
}

/// What the player chose for one activation, by its name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivationSettings {
    pub mode: Mode,
    /// Its key toggles instead of being held.
    pub toggle: bool,
    /// How far it carries, when the server allows it (0: the server's default).
    pub distance: i32,
}

/// What the player chose.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Hear nobody and send nothing (Plasmo Voice's "voice disabled").
    pub deafened: bool,
    pub microphone_muted: bool,
    /// Voice activation's level, in dBFS (Plasmo Voice: -30).
    pub threshold_db: f32,
    /// 0 to 2.
    pub volume: f32,
    pub microphone_gain: f32,
    /// Device names; None is the system's default.
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// By activation name; proximity's mode is what `Inherit` follows.
    pub activations: HashMap<String, ActivationSettings>,
    /// By source line name (`proximity`, `groups`…), 0 to 2: else the line's default.
    pub line_volumes: HashMap<String, f32>,
    /// By player UUID, 0 to 2.
    pub player_volumes: HashMap<u128, f32>,
    /// Players this client doesn't hear.
    pub muted_players: HashSet<u128>,
    pub noise_gate: bool,
    pub noise_gate_db: f32,
    pub compressor: bool,
    pub limiter: bool,
    /// Distance gain cubed (`exponentialDistanceGain`): quieter sooner.
    pub exponential_distance: bool,
    /// Blocks between a source and the listener muffle it.
    pub occlusion: bool,
    /// Louder in the ear on a source's side; off, only the distance sets the volume.
    pub panning: bool,
    /// Hear your own microphone after its filters.
    pub monitor: bool,
    /// Don't open the microphone at all (`Disable Input Device`).
    pub input_disabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            deafened: false,
            microphone_muted: false,
            threshold_db: -30.0,
            volume: 1.0,
            microphone_gain: 1.0,
            input_device: None,
            output_device: None,
            activations: HashMap::new(),
            line_volumes: HashMap::new(),
            player_volumes: HashMap::new(),
            muted_players: HashSet::new(),
            noise_gate: false,
            noise_gate_db: -50.0,
            compressor: false,
            limiter: true,
            // Plasmo Voice's default (`exponentialDistanceGain`): voices fade well before
            // their distance.
            exponential_distance: true,
            occlusion: false,
            panning: false,
            monitor: false,
            input_disabled: false,
        }
    }
}

impl Settings {
    /// An activation's settings: proximity speaks with its key, the others as proximity.
    pub fn activation(&self, name: &str) -> ActivationSettings {
        self.activations.get(name).cloned().unwrap_or(ActivationSettings {
            mode: if name == proto::PROXIMITY { Mode::PushToTalk } else { Mode::Inherit },
            ..Default::default()
        })
    }
    /// An activation's mode with `Inherit` resolved.
    pub fn mode(&self, name: &str) -> Mode {
        match self.activation(name).mode {
            Mode::Inherit if name != proto::PROXIMITY => match self.activation(proto::PROXIMITY).mode {
                Mode::Inherit => Mode::PushToTalk,
                m => m,
            },
            Mode::Inherit => Mode::PushToTalk,
            m => m,
        }
    }
}

/// Where a source is, for the game to answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Locator {
    /// A player, by profile UUID.
    Player(u128),
    /// An entity, by network id.
    Entity(i32),
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
    /// No voice server on this game server (yet).
    Off,
    /// Told where the voice server is; no answer yet.
    Connecting,
    Connected,
    /// It stopped answering.
    TimedOut,
}

/// An activation as the menu shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivationView {
    pub name: String,
    pub label: String,
    pub icon: String,
    pub distances: Vec<i32>,
    pub default_distance: i32,
    /// The distance it speaks at now.
    pub distance: i32,
    pub proximity: bool,
}

/// A source line as the menu shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct LineView {
    pub name: String,
    pub label: String,
    pub icon: String,
    pub default_volume: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub connection: Connection,
    /// The activations sending right now.
    pub transmitting: Vec<String>,
    /// The microphone's last frame, in dBFS (after the gain).
    pub microphone_db: f32,
    /// Who is heard right now: name, and the player's UUID when it is one.
    pub speaking: Vec<(String, Option<u128>)>,
    pub microphone: Option<String>,
    pub speakers: Option<String>,
}

/// The voice server's settings once the config came: what the encoder and decoders need.
struct Voice {
    rate: i32,
    frame: usize,
    aes: Option<Aes128>,
    capture_encoder: Option<CodecInfo>,
}

/// One connection to a voice server, shared by its threads.
struct Link {
    socket: UdpSocket,
    secret: u128,
    stop: AtomicBool,
    /// The last ping from the server, in ms since the epoch; 0 before the first.
    keepalive: AtomicI64,
    voice: Mutex<Option<Arc<Voice>>>,
    settings: Mutex<Arc<Settings>>,
    activations: Mutex<Vec<Activation>>,
    /// Activations whose key is down (or toggled on), by name.
    held: Mutex<HashSet<String>>,
    sources: Mutex<HashMap<u128, SourceInfo>>,
    /// Sources heard before their info: the game thread asks the server about them.
    wanted: Mutex<HashSet<u128>>,
    decoders: Mutex<HashMap<u128, (Decoder, i64)>>,
    /// Sources in a codec this client lacks.
    unplayable: Mutex<HashSet<u128>>,
    mixer: Arc<Mutex<Mixer>>,
    /// Payloads for the game to send (from the encoder: the ends of streams).
    outgoing: Mutex<Vec<Vec<u8>>>,
    errors: Mutex<Vec<String>>,
    transmitting: Mutex<Vec<String>>,
    microphone_db: AtomicU32,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl Link {
    fn connection(&self) -> Connection {
        match self.keepalive.load(Ordering::Relaxed) {
            0 => Connection::Connecting,
            k if now_ms() - k > LOST_MS => Connection::Connecting,
            k if now_ms() - k > SOFT_TIMEOUT_MS => Connection::TimedOut,
            _ => Connection::Connected,
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

/// The distance `a` speaks at: the player's choice if the server allows it, else its default.
fn distance_of(a: &Activation, s: &Settings) -> i32 {
    let d = s.activation(&a.name).distance;
    if d > 0 && a.allows(d) { d } else { a.default_distance }
}

/// The microphone, the speakers and the encoder of a configured link.
struct Devices {
    capture: Option<Stream>,
    playback: Option<Stream>,
    encoder: Option<JoinHandle<()>>,
}

pub struct Engine {
    minecraft_version: String,
    language: String,
    key: Option<Arc<dyn RsaKey>>,
    settings: Settings,
    held: HashSet<String>,
    /// The game server's host, for a voice server at 0.0.0.0.
    server_host: String,
    link: Option<Arc<Link>>,
    net: Option<JoinHandle<()>>,
    devices: Option<Devices>,
    config: Option<Config>,
    players: HashMap<u128, VoicePlayer>,
    translations: HashMap<String, String>,
    /// The server asked who we are before the key was ready.
    info_requested: bool,
    asked: HashMap<u128, Instant>,
    outgoing: Vec<Vec<u8>>,
    messages: Vec<String>,
    action_bar: Option<String>,
    shown_distance: Option<(i32, Instant)>,
    /// How each source heard was placed at the last tick (`/vc debug`).
    placed: Vec<String>,
    ticks: u64,
}

impl Engine {
    pub fn new(settings: Settings, minecraft_version: &str) -> Self {
        Self {
            minecraft_version: minecraft_version.into(),
            language: "en_us".into(),
            key: None,
            settings,
            held: HashSet::new(),
            server_host: String::new(),
            link: None,
            net: None,
            devices: None,
            config: None,
            players: HashMap::new(),
            translations: HashMap::new(),
            info_requested: false,
            asked: HashMap::new(),
            outgoing: Vec::new(),
            messages: Vec::new(),
            action_bar: None,
            shown_distance: None,
            placed: Vec::new(),
            ticks: 0,
        }
    }

    pub fn set_key(&mut self, key: Arc<dyn RsaKey>) {
        self.key = Some(key);
    }
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    /// The game's language (`ru_ru`): the server's translations come in it.
    pub fn set_language(&mut self, language: &str) {
        if self.language != language {
            self.language = language.to_string();
            if self.config.is_some() {
                self.outgoing.push(serverbound::language_request(language));
            }
        }
    }

    /// New settings: they apply at once, devices reopen when theirs changed, and the server
    /// hears of a mute and of new distances.
    pub fn set_settings(&mut self, settings: Settings) {
        let old = std::mem::replace(&mut self.settings, settings.clone());
        if let Some(link) = &self.link {
            *link.settings.lock().unwrap() = Arc::new(settings.clone());
            if let Ok(mut m) = link.mixer.lock() {
                m.master = settings.volume;
                if settings.deafened {
                    m.clear();
                }
            }
            if (old.deafened, old.microphone_muted) != (settings.deafened, settings.microphone_muted) {
                self.outgoing.push(serverbound::player_state(settings.deafened, settings.microphone_muted));
            }
            if old.activations != settings.activations {
                self.send_distances();
            }
        }
        if (old.input_device, old.output_device, old.input_disabled)
            != (settings.input_device, settings.output_device, settings.input_disabled)
            && self.devices.is_some()
        {
            self.devices = None;
            self.start_devices();
        }
    }

    /// An activation's key went down or up (or, toggled, on or off).
    pub fn set_held(&mut self, activation: &str, down: bool) {
        if down {
            self.held.insert(activation.to_string());
        } else {
            self.held.remove(activation);
        }
        if let Some(link) = &self.link {
            *link.held.lock().unwrap() = self.held.clone();
        }
    }
    pub fn is_held(&self, activation: &str) -> bool {
        self.held.contains(activation)
    }
    /// Every key up: the player opened a screen.
    pub fn release_keys(&mut self) {
        let toggled: Vec<String> = self
            .held
            .iter()
            .filter(|a| self.settings.activation(a).toggle)
            .cloned()
            .collect();
        self.held = toggled.into_iter().collect();
        if let Some(link) = &self.link {
            *link.held.lock().unwrap() = self.held.clone();
        }
    }

    /// A payload of `plasmo:voice/v2` from the server.
    pub fn receive(&mut self, data: &[u8]) {
        let packet = match Clientbound::decode(data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Proximity Voice: bad packet {:?}: {}", data.first(), e.0);
                return;
            }
        };
        match packet {
            Clientbound::PlayerInfoRequest => {
                self.info_requested = true;
                self.answer_info_request();
            }
            Clientbound::Connection { secret, ip, port } => self.connect(secret, &ip, port),
            Clientbound::Config(config) => self.configure(*config),
            Clientbound::ConfigPlayerInfo(permissions) => {
                if let Some(c) = &mut self.config {
                    c.permissions = permissions;
                }
            }
            Clientbound::PlayerList(players) => {
                self.players = players.into_iter().map(|p| (p.id, p)).collect();
            }
            Clientbound::PlayerInfoUpdate(p) => {
                self.players.insert(p.id, p);
            }
            Clientbound::PlayerDisconnect(id) => {
                self.players.remove(&id);
            }
            Clientbound::SourceInfo(info) => {
                if let Some(link) = &self.link {
                    let changed = link.sources.lock().unwrap().insert(info.id, (*info).clone());
                    if changed.is_some_and(|old| old.decoder != info.decoder || old.stereo != info.stereo) {
                        link.decoders.lock().unwrap().remove(&info.id);
                    }
                    link.wanted.lock().unwrap().remove(&info.id);
                }
            }
            Clientbound::SourceAudioEnd { source, .. } => {
                if let Some(link) = &self.link {
                    link.mixer.lock().unwrap().end(source);
                    link.decoders.lock().unwrap().remove(&source);
                }
            }
            Clientbound::ActivationRegister(a) => {
                if let Some(link) = &self.link {
                    let mut list = link.activations.lock().unwrap();
                    list.retain(|x| x.id != a.id);
                    list.push(*a);
                }
                self.send_distances();
            }
            Clientbound::ActivationUnregister(id) => {
                if let Some(link) = &self.link {
                    link.activations.lock().unwrap().retain(|x| x.id != id);
                }
            }
            Clientbound::SourceLineRegister(line) => {
                if let Some(c) = &mut self.config {
                    c.lines.retain(|x| x.id != line.id);
                    c.lines.push(line);
                }
            }
            Clientbound::SourceLineUnregister(id) => {
                if let Some(c) = &mut self.config {
                    c.lines.retain(|x| x.id != id);
                }
            }
            Clientbound::Language(entries) => self.translations.extend(entries),
            Clientbound::ActionBar(json) => self.action_bar = Some(json),
            Clientbound::DistanceVisualize { radius, .. } => self.shown_distance = Some((radius, Instant::now())),
            Clientbound::SelfSourceInfo { .. } | Clientbound::Ignored(_) => {}
        }
    }

    fn answer_info_request(&mut self) {
        let Some(key) = &self.key else { return };
        if !self.info_requested {
            return;
        }
        self.info_requested = false;
        self.outgoing.push(serverbound::player_info(
            self.settings.deafened,
            self.settings.microphone_muted,
            &self.minecraft_version,
            PROTOCOL_VERSION,
            &key.public_der(),
        ));
    }

    /// `ConnectionPacket`: a fresh UDP link that pings the voice server until it answers.
    fn connect(&mut self, secret: u128, ip: &str, port: i32) {
        self.close_link();
        let host = if ip == "0.0.0.0" { self.server_host.clone() } else { ip.to_string() };
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
        let link = Arc::new(Link {
            socket,
            secret,
            stop: AtomicBool::new(false),
            keepalive: AtomicI64::new(0),
            voice: Mutex::new(None),
            settings: Mutex::new(Arc::new(self.settings.clone())),
            activations: Mutex::new(Vec::new()),
            held: Mutex::new(self.held.clone()),
            sources: Mutex::new(HashMap::new()),
            wanted: Mutex::new(HashSet::new()),
            decoders: Mutex::new(HashMap::new()),
            unplayable: Mutex::new(HashSet::new()),
            mixer: Arc::new(Mutex::new(Mixer::new(48000))),
            outgoing: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            transmitting: Mutex::new(Vec::new()),
            microphone_db: AtomicU32::new((-100f32).to_bits()),
        });
        let reader = link.clone();
        self.net = std::thread::Builder::new()
            .name("proximity-voice-udp".into())
            .spawn(move || udp_loop(reader, host, port))
            .ok();
        self.link = Some(link);
    }

    /// `ConfigPacket`: the key, the rate, the activations; then the microphone and speakers.
    fn configure(&mut self, config: Config) {
        let Some(link) = self.link.clone() else {
            eprintln!("Proximity Voice: config before the voice server's address");
            return;
        };
        let aes = match &config.encryption {
            None => None,
            Some((algorithm, data)) if algorithm == "AES/CBC/PKCS5Padding" => {
                let key = self.key.as_ref().and_then(|k| k.decrypt(data));
                match key.as_deref().and_then(|k| <[u8; 16]>::try_from(k).ok()) {
                    Some(k) => Some(Aes128::new(&k)),
                    None => {
                        self.messages.push("Voice chat: couldn't read the server's voice key".into());
                        self.close_link();
                        return;
                    }
                }
            }
            Some((algorithm, _)) => {
                self.messages.push(format!("Voice chat: the server encrypts with {algorithm}, which this client lacks"));
                self.close_link();
                return;
            }
        };
        let rate = config.sample_rate;
        if ![8000, 12000, 16000, 24000, 48000].contains(&rate) {
            self.messages.push(format!("Voice chat: the server's sample rate {rate} isn't supported"));
            return;
        }
        let voice = Voice { rate, frame: rate as usize / 50, aes, capture_encoder: config.capture_encoder.clone() };
        {
            let mut mixer = link.mixer.lock().unwrap();
            *mixer = Mixer::new(rate as u32);
            mixer.master = self.settings.volume;
        }
        link.decoders.lock().unwrap().clear();
        *link.activations.lock().unwrap() = config.activations.clone();
        *link.voice.lock().unwrap() = Some(Arc::new(voice));
        self.config = Some(config);
        self.send_distances();
        self.outgoing.push(serverbound::language_request(&self.language));
        self.devices = None;
        self.start_devices();
    }

    /// `PlayerActivationDistancesPacket`: each activation's distance.
    fn send_distances(&mut self) {
        let Some(link) = &self.link else { return };
        let list: Vec<(u128, i32)> = link
            .activations
            .lock()
            .unwrap()
            .iter()
            .map(|a| (a.id, distance_of(a, &self.settings)))
            .collect();
        if !list.is_empty() {
            self.outgoing.push(serverbound::activation_distances(&list));
        }
    }

    fn start_devices(&mut self) {
        let Some(link) = self.link.clone() else { return };
        let Some(voice) = link.voice.lock().unwrap().clone() else { return };
        let mut devices = Devices { capture: None, playback: None, encoder: None };
        match audio::playback(self.settings.output_device.clone(), link.mixer.clone()) {
            Ok(s) => devices.playback = Some(s),
            Err(e) => self.messages.push(format!("Voice chat: can't open the speakers: {e}")),
        }
        if self.settings.input_disabled {
            self.devices = Some(devices);
            return;
        }
        let (sink, blocks) = mpsc::sync_channel(64);
        match audio::capture(self.settings.input_device.clone(), sink) {
            Ok(s) => {
                let (rate, encoder_link) = (s.rate, link.clone());
                devices.encoder = std::thread::Builder::new()
                    .name("proximity-voice-encoder".into())
                    .spawn(move || encode_loop(encoder_link, voice, blocks, rate))
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
        self.config = None;
        self.asked.clear();
    }

    /// The game left the server: everything goes.
    pub fn reset(&mut self) {
        self.close_link();
        self.players.clear();
        self.translations.clear();
        self.info_requested = false;
        self.outgoing.clear();
    }

    /// Once a tick: `server_host` is the game server's host, `listener` the player (None out of
    /// a world), `locate` finds players and entities, `occluded` says how many solid blocks lie
    /// between two points (up to a few).
    pub fn tick(
        &mut self,
        server_host: &str,
        listener: Option<Listener>,
        locate: &mut dyn FnMut(Locator) -> Option<[f64; 3]>,
        occluded: &mut dyn FnMut([f64; 3], [f64; 3]) -> u32,
    ) {
        if self.server_host != server_host {
            self.server_host = server_host.to_string();
        }
        self.answer_info_request();
        self.ticks += 1;
        let Some(link) = self.link.clone() else { return };
        self.outgoing.append(&mut link.outgoing.lock().unwrap());
        self.messages.append(&mut link.errors.lock().unwrap());
        // Ask about unknown sources, once a second each.
        let wanted: Vec<u128> = link.wanted.lock().unwrap().iter().copied().collect();
        for source in wanted {
            if self.asked.get(&source).is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
                self.asked.insert(source, Instant::now());
                self.outgoing.push(serverbound::source_info_request(source));
            }
        }
        // Place what is heard.
        let s = &self.settings;
        let heard = link.mixer.lock().unwrap().heard(Duration::from_secs(2));
        let sources = link.sources.lock().unwrap();
        let mut gains = Vec::with_capacity(heard.len());
        let mut placed = Vec::new();
        for (id, distance) in heard {
            let Some(info) = sources.get(&id) else { continue };
            let line = self.config.as_ref().and_then(|c| c.lines.iter().find(|l| l.id == info.line_id));
            let mut volume = line.map_or(1.0, |l| {
                s.line_volumes.get(&l.name).copied().unwrap_or(l.default_volume as f32)
            });
            let player = match &info.kind {
                SourceKind::Player(p) => Some(p.id),
                SourceKind::Direct { sender: Some(p), .. } => Some(p.id),
                _ => None,
            };
            if let Some(p) = player {
                if s.muted_players.contains(&p) {
                    volume = 0.0;
                }
                volume *= s.player_volumes.get(&p).copied().unwrap_or(1.0);
            }
            let at = match &info.kind {
                SourceKind::Player(p) => locate(Locator::Player(p.id)),
                SourceKind::Entity(e) => locate(Locator::Entity(*e)),
                SourceKind::Static(at) => Some(*at),
                SourceKind::Direct { relative: Some(r), .. } => {
                    listener.map(|l| [l.position[0] + r[0], l.position[1] + r[1], l.position[2] + r[2]])
                }
                SourceKind::Direct { relative: None, .. } => None,
            };
            if s.occlusion
                && let (Some(l), Some(at)) = (listener, at)
            {
                // Each block between takes a fifth, down to a fifth of the voice.
                volume *= 1.0 - 0.2 * occluded(l.position, at).min(4) as f32;
            }
            let [l, r] = spatial(listener, at, distance as f64, s.exponential_distance, s.panning);
            let kind = match &info.kind {
                SourceKind::Player(p) => format!("player {} {:032x}", p.name, p.id),
                SourceKind::Entity(e) => format!("entity {e}"),
                SourceKind::Static(_) => "static".into(),
                SourceKind::Direct { .. } => "direct".into(),
            };
            let away = match (listener, at) {
                (Some(l), Some(a)) => format!(
                    "{:.1} blocks away",
                    ((a[0] - l.position[0]).powi(2) + (a[1] - l.position[1]).powi(2) + (a[2] - l.position[2]).powi(2)).sqrt()
                ),
                (None, _) => "no listener".into(),
                (_, None) => "NOT FOUND in the world".into(),
            };
            placed.push(format!("{kind}: {away}, carries {distance}, volume {volume:.2}, left {l:.2} right {r:.2}"));
            gains.push((id, [l * volume, r * volume]));
        }
        drop(sources);
        self.placed = placed;
        let mut mixer = link.mixer.lock().unwrap();
        for (id, g) in gains {
            mixer.set_gains(id, g);
        }
    }

    /// Payloads to send on `plasmo:voice/v2`.
    pub fn take_outgoing(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outgoing)
    }
    /// Lines for the player's chat.
    pub fn take_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.messages)
    }
    /// A JSON text component for the action bar (`AnimatedActionBarPacket`).
    pub fn take_action_bar(&mut self) -> Option<String> {
        self.action_bar.take()
    }
    /// A distance the server or the player just set, for a moment.
    pub fn shown_distance(&self) -> Option<i32> {
        self.shown_distance
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(3))
            .map(|(d, _)| d)
    }
    pub fn show_distance(&mut self, distance: i32) {
        self.shown_distance = Some((distance, Instant::now()));
    }

    /// How each source heard at the last tick was placed, and how many ticks ran.
    pub fn debug(&self) -> (u64, Vec<String>) {
        (self.ticks, self.placed.clone())
    }

    /// The server's translation of `key`, else a readable name from its last part.
    pub fn translate(&self, key: &str, name: &str) -> String {
        if let Some(t) = self.translations.get(key) {
            return t.clone();
        }
        let mut c = name.chars();
        match c.next() {
            Some(f) => f.to_uppercase().chain(c).collect::<String>().replace(['_', '-'], " "),
            None => key.to_string(),
        }
    }

    /// The server's activations, heaviest first.
    pub fn activations(&self) -> Vec<ActivationView> {
        let Some(link) = &self.link else { return Vec::new() };
        let mut list: Vec<Activation> = link.activations.lock().unwrap().clone();
        list.sort_by_key(|a| (-a.weight, a.name.clone()));
        list.iter()
            .map(|a| ActivationView {
                name: a.name.clone(),
                label: self.translate(&a.translation, &a.name),
                icon: a.icon.clone(),
                distances: a.distances.clone(),
                default_distance: a.default_distance,
                distance: distance_of(a, &self.settings),
                proximity: a.name == proto::PROXIMITY,
            })
            .collect()
    }

    pub fn lines(&self) -> Vec<LineView> {
        let Some(c) = &self.config else { return Vec::new() };
        let mut lines: Vec<LineView> = c
            .lines
            .iter()
            .map(|l| LineView {
                name: l.name.clone(),
                label: self.translate(&l.translation, &l.name),
                icon: l.icon.clone(),
                default_volume: l.default_volume as f32,
            })
            .collect();
        lines.sort_by(|a, b| a.name.cmp(&b.name));
        lines
    }

    /// The players with voice chat, by name.
    pub fn players(&self) -> Vec<VoicePlayer> {
        let mut list: Vec<VoicePlayer> = self.players.values().cloned().collect();
        list.sort_by_key(|p| p.name.to_lowercase());
        list
    }
    pub fn player(&self, id: u128) -> Option<&VoicePlayer> {
        self.players.get(&id)
    }

    pub fn status(&self) -> Status {
        let Some(link) = &self.link else {
            return Status {
                connection: Connection::Off,
                transmitting: Vec::new(),
                microphone_db: -100.0,
                speaking: Vec::new(),
                microphone: None,
                speakers: None,
            };
        };
        let sources = link.sources.lock().unwrap();
        let mut speaking: Vec<(String, Option<u128>)> = link
            .mixer
            .lock()
            .unwrap()
            .heard(Duration::from_millis(300))
            .into_iter()
            .filter_map(|(id, _)| sources.get(&id))
            .map(|s| match &s.kind {
                SourceKind::Player(p) => (p.name.clone(), Some(p.id)),
                SourceKind::Direct { sender: Some(p), .. } => (p.name.clone(), Some(p.id)),
                _ => (s.name.clone().unwrap_or_else(|| "?".into()), None),
            })
            .collect();
        speaking.sort();
        speaking.dedup();
        Status {
            connection: link.connection(),
            transmitting: link.transmitting.lock().unwrap().clone(),
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

/// The gain of a mono source in front in each ear: what OpenAL gives Plasmo Voice's sources.
pub const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Left and right gains of a source at `at` heard by `listener`: linear falloff to
/// `max_distance` (Plasmo Voice's `calculateDistanceGain`, cubed when `exponential`) and
/// with `panning`, equal-power panning by the side it is on. A source with no place plays in the middle at
/// the gain OpenAL gives it in front. The gains are a mono source's: a stereo one plays
/// each channel `sqrt(2)` louder (the mixer's `STEREO`), as OpenAL leaves stereo unpanned.
pub fn spatial(
    listener: Option<Listener>,
    at: Option<[f64; 3]>,
    max_distance: f64,
    exponential: bool,
    panning: bool,
) -> [f32; 2] {
    let (Some(l), Some(at)) = (listener, at) else { return [CENTRE, CENTRE] };
    let d = [at[0] - l.position[0], at[1] - l.position[1], at[2] - l.position[2]];
    let distance = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let mut falloff = if max_distance > 0.0 { 1.0 - distance.min(max_distance) / max_distance } else { 1.0 };
    if exponential {
        falloff = falloff.powi(3);
    }
    let horizontal = (d[0] * d[0] + d[2] * d[2]).sqrt();
    let pan = if !panning || horizontal < 0.5 {
        0.0
    } else {
        // The listener's right: west when facing south.
        let yaw = (l.yaw as f64).to_radians();
        let right = [-yaw.cos(), -yaw.sin()];
        ((d[0] * right[0] + d[2] * right[1]) / horizontal).clamp(-1.0, 1.0)
    };
    // Equal-power panning as OpenAL's: a mono source in front is 1/sqrt(2) in each ear.
    let left = ((1.0 - pan) / 2.0).sqrt() * falloff;
    let right = ((1.0 + pan) / 2.0).sqrt() * falloff;
    [left as f32, right as f32]
}

/// The UDP reader: pings until the server answers, answers its pings, and decodes the sources
/// it sends into the mixer.
fn udp_loop(link: Arc<Link>, host: String, port: u16) {
    let mut buf = vec![0u8; 4096];
    let mut last_ping: Option<Instant> = None;
    while !link.stop.load(Ordering::Relaxed) {
        if link.connection() == Connection::Connecting && last_ping.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            let _ = link.socket.send(&proto::udp_ping(link.secret, now_ms(), Some((&host, port))));
            last_ping = Some(Instant::now());
        }
        // Errors are timeouts, or (Windows) the ICMP of a port that isn't open yet.
        let Ok(n) = link.socket.recv(&mut buf) else { continue };
        match UdpIn::decode(&buf[..n]) {
            Ok(UdpIn::Ping) => {
                link.keepalive.store(now_ms(), Ordering::Relaxed);
                let _ = link.socket.send(&proto::udp_ping(link.secret, now_ms(), None));
            }
            Ok(UdpIn::SourceAudio { sequence, data, source, state, distance }) => {
                hear(&link, sequence, &data, source, state, distance);
            }
            Ok(UdpIn::Other) | Err(_) => {}
        }
    }
}

fn hear(link: &Link, sequence: i64, data: &[u8], source: u128, state: u8, distance: i16) {
    if link.settings().deafened {
        return;
    }
    let Some(voice) = link.voice.lock().unwrap().clone() else { return };
    let info = link.sources.lock().unwrap().get(&source).cloned();
    let Some(info) = info else {
        link.wanted.lock().unwrap().insert(source);
        return;
    };
    if info.state != state {
        // The source changed (`SourceAudioPacket.sourceState`): ask again, keep playing.
        link.wanted.lock().unwrap().insert(source);
    }
    let plain = match &voice.aes {
        Some(aes) => match cbc_decrypt(aes, data) {
            Some(p) => p,
            None => return,
        },
        None => data.to_vec(),
    };
    let mut decoders = link.decoders.lock().unwrap();
    let (decoder, last) = match decoders.entry(source) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => {
            match Decoder::new(info.decoder.as_ref(), voice.rate, voice.frame, info.stereo) {
                Ok(d) => e.insert((d, sequence - 1)),
                Err(err) => {
                    // Said once per source: its packets are dropped from then on.
                    if link.unplayable.lock().unwrap().insert(source) {
                        link.error(format!("Voice chat: {err}"));
                    }
                    return;
                }
            }
        }
    };
    if sequence <= *last {
        // Late or repeated.
        return;
    }
    let mut pcm = Vec::new();
    // A few lost packets are concealed; a longer gap just restarts.
    if (2..=4).contains(&(sequence - *last)) {
        for _ in *last + 1..sequence {
            if let Some(p) = decoder.decode(&[]) {
                pcm.extend(p);
            }
        }
    }
    *last = sequence;
    if let Some(p) = decoder.decode(&plain) {
        pcm.extend(p);
    }
    let channels = decoder.channels();
    drop(decoders);
    link.mixer.lock().unwrap().push(source, channels, &pcm, distance);
}

/// One activation's stream: its encoder and sequence numbers.
struct Outgoing {
    encoder: Encoder,
    sequence: i64,
    active: bool,
}

/// The encoder: the microphone's blocks, resampled to the server's rate, cut into 20 ms
/// frames and filtered, go out as voice packets of each activation that speaks (heaviest
/// first: one that isn't transitive keeps the frame from the lighter ones); the end of each
/// stream goes to the server over the game connection (`PlayerAudioEndPacket`).
fn encode_loop(link: Arc<Link>, voice: Arc<Voice>, blocks: mpsc::Receiver<Vec<f32>>, device_rate: u32) {
    let mut resampler = Resampler::new(device_rate, voice.rate as u32);
    let mut pending: Vec<f32> = Vec::new();
    let mut streams: HashMap<u128, Outgoing> = HashMap::new();
    let mut hangover = 0u32;
    let mut gate = NoiseGate::default();
    let mut compressor = Compressor::new(voice.rate as u32);
    let mut warned = false;
    loop {
        let block = match blocks.recv_timeout(Duration::from_millis(200)) {
            Ok(b) => b,
            Err(RecvTimeoutError::Timeout) if !link.stop.load(Ordering::Relaxed) => continue,
            _ => break,
        };
        resampler.process(&block, &mut pending);
        while pending.len() >= voice.frame {
            let s = link.settings();
            let mut frame: Vec<f32> = pending.drain(..voice.frame).map(|x| x * s.microphone_gain).collect();
            if s.noise_gate {
                gate.process(&mut frame, s.noise_gate_db);
            }
            if s.compressor {
                compressor.process(&mut frame);
            }
            if s.limiter {
                dsp::limit(&mut frame);
            }
            for x in frame.iter_mut() {
                *x = x.clamp(-1.0, 1.0);
            }
            let db = dsp::level_db(&frame);
            link.microphone_db.store(db.to_bits(), Ordering::Relaxed);
            if s.monitor {
                link.mixer.lock().unwrap().push(MONITOR, 1, &frame, 0);
            }
            hangover = if db > s.threshold_db { HANGOVER_FRAMES } else { hangover.saturating_sub(1) };
            let allowed = !s.microphone_muted && !s.deafened && link.connection() == Connection::Connected;
            let held = link.held.lock().unwrap().clone();
            let mut activations = link.activations.lock().unwrap().clone();
            activations.sort_by_key(|a| -a.weight);
            let mut blocked = false;
            let mut sending = Vec::new();
            for a in &activations {
                let wants = allowed
                    && !blocked
                    && match s.mode(&a.name) {
                        crate::engine::Mode::Voice => hangover > 0,
                        _ => held.contains(&a.name),
                    };
                let stream = match streams.entry(a.id) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let codec = a.encoder.as_ref().or(voice.capture_encoder.as_ref());
                        match Encoder::new(codec, voice.rate, voice.frame) {
                            Ok(encoder) => e.insert(Outgoing { encoder, sequence: 0, active: false }),
                            Err(err) => {
                                if !warned {
                                    warned = true;
                                    link.error(format!("Voice chat: {err}"));
                                }
                                continue;
                            }
                        }
                    }
                };
                let distance = distance_of(a, &s) as i16;
                if wants {
                    let mut f = frame.clone();
                    if !stream.active {
                        // A stream starts from silence: no click.
                        f.iter_mut().take(48).enumerate().for_each(|(i, x)| *x *= i as f32 / 48.0);
                    }
                    let Some(mut packet) = stream.encoder.encode(&f) else { continue };
                    if let Some(aes) = &voice.aes {
                        let mut iv = [0u8; 16];
                        if getrandom::getrandom(&mut iv).is_err() {
                            continue;
                        }
                        packet = cbc_encrypt(aes, iv, &packet);
                    }
                    stream.sequence += 1;
                    let _ = link.socket.send(&proto::udp_player_audio(
                        link.secret,
                        now_ms(),
                        stream.sequence,
                        &packet,
                        a.id,
                        distance,
                        false,
                    ));
                    stream.active = true;
                    sending.push(a.name.clone());
                    if !a.transitive {
                        blocked = true;
                    }
                } else if stream.active {
                    stream.sequence += 1;
                    link.outgoing.lock().unwrap().push(serverbound::audio_end(stream.sequence, a.id, distance));
                    stream.encoder.reset();
                    stream.active = false;
                }
            }
            *link.transmitting.lock().unwrap() = sending;
        }
    }
    link.transmitting.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panning_follows_the_listener() {
        // Facing south (+z), a source to the west (-x) is on the right.
        let l = Some(Listener { position: [0.0; 3], yaw: 0.0 });
        let [left, right] = spatial(l, Some([-8.0, 0.0, 0.0]), 16.0, false, true);
        assert!(left < 0.01 && (right - 0.5).abs() < 0.01, "{left} {right}");
        // Facing north, the same source is on the left.
        let l = Some(Listener { position: [0.0; 3], yaw: 180.0 });
        let [left, right] = spatial(l, Some([-8.0, 0.0, 0.0]), 16.0, false, true);
        assert!(right < 0.01 && (left - 0.5).abs() < 0.01, "{left} {right}");
        // In front: both sides at -3 dB; past the distance: silent; nowhere: as in front.
        let [left, right] = spatial(l, Some([0.0, 0.0, -4.0]), 16.0, false, true);
        assert!((left - 0.75 * CENTRE).abs() < 1e-4 && (right - left).abs() < 1e-4);
        assert_eq!(spatial(l, Some([0.0, 0.0, -40.0]), 16.0, false, true), [0.0, 0.0]);
        assert_eq!(spatial(l, None, 16.0, false, true), [CENTRE, CENTRE]);
        // Without panning, a source to one side is as loud in both ears.
        let [left, right] = spatial(l, Some([-8.0, 0.0, 0.0]), 16.0, false, false);
        assert!((left - 0.5 * CENTRE).abs() < 1e-4 && (right - left).abs() < 1e-4);
        // Exponential: (3/4)^3.
        let [left, _] = spatial(l, Some([0.0, 0.0, -4.0]), 16.0, true, true);
        assert!((left - 0.421875 * CENTRE).abs() < 1e-4);
    }

    #[test]
    fn inherit_follows_proximity() {
        let mut s = Settings::default();
        assert_eq!(s.mode("proximity"), Mode::PushToTalk);
        assert_eq!(s.mode("groups"), Mode::PushToTalk);
        s.activations.insert("proximity".into(), ActivationSettings { mode: Mode::Voice, ..Default::default() });
        assert_eq!(s.mode("groups"), Mode::Voice);
        s.activations.insert("groups".into(), ActivationSettings { mode: Mode::PushToTalk, ..Default::default() });
        assert_eq!(s.mode("groups"), Mode::PushToTalk);
    }

    #[test]
    fn translations_fall_back_to_the_name() {
        let mut e = Engine::new(Settings::default(), "26.3");
        assert_eq!(e.translate("pv.activation.groups", "groups"), "Groups");
        e.translations.insert("pv.activation.groups".into(), "Группы".into());
        assert_eq!(e.translate("pv.activation.groups", "groups"), "Группы");
    }
}
