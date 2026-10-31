// Proximity Voice: a Plasmo Voice 2.x client for Horizon. This module is the game's side of
// it (crate::game::proximity_voice): it registers the plugin channel, hands the server's
// payloads to the engine (the proximity-voice-engine crate) and sends its answers, tells it
// where the player and the other sources are and what lies between, and draws, binds and keeps
// what the player sees and chooses: the HUD, the marks over heads, the keys of every
// activation and the menu (server-style dialogs, with sliders, run by `/vc` commands). The
// engine does the protocol, the UDP link, Opus, the filters and the audio devices.
//
// A port of Plasmo Voice's client (https://github.com/plasmoapp/plasmo-voice, LGPL-3.0-only),
// under the same licence.
use horizon_protocol::packets::{configuration::CustomPayload, play};
use proximity_voice_engine::{self as voice, ActivationSettings, Connection, Engine, Listener, Locator, Mode, Settings};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, Once, OnceLock};

pub const ID: &str = "proximity_voice";
pub const VERSION: &str = "0.4.1";
/// The game version Plasmo Voice's server is told (`PlayerInfoPacket.minecraftVersion`).
const MINECRAFT: &str = "26.3";
/// Keys: proximity's talk key, then the menu, mute and deafen. Other activations get theirs
/// as `act.<name>`, unbound until the player binds them.
pub const TALK: &str = "talk";
pub const TALK_DEFAULT: &str = "key.keyboard.left.alt";
pub const MENU: &str = "menu";
pub const MENU_DEFAULT: &str = "key.keyboard.v";
pub const MUTE: &str = "mute";
pub const MUTE_DEFAULT: &str = "key.keyboard.m";
pub const DEAFEN: &str = "deafen";
pub const DEAFEN_DEFAULT: &str = "key.keyboard.unknown";
pub const DISTANCE_UP: &str = "distance_up";
pub const DISTANCE_DOWN: &str = "distance_down";
pub const OCCLUSION: &str = "occlusion";
/// Eye height of a standing player: voices come from heads.
const EYES: f64 = 1.62;
const WHITE: u32 = 0xFFFFFF;

/// Where the HUD box goes, and what else is drawn.
#[derive(Clone, Debug, PartialEq)]
struct Overlay {
    shown: bool,
    corner: usize,
    nametags: bool,
    speakers: bool,
}

impl Default for Overlay {
    fn default() -> Self {
        Self { shown: true, corner: 0, nametags: true, speakers: true }
    }
}

// Russian, when the game is in Russian: every line the player reads, by its English.
const RUSSIAN: &[(&str, &str)] = &[
    ("Devices", "Устройства"),
    ("Volume", "Громкость"),
    ("Activations", "Активации"),
    ("Overlay", "Оверлей"),
    ("Advanced", "Продвинутые"),
    ("Keys", "Клавиши"),
    ("Microphone", "Микрофон"),
    ("Activation threshold", "Порог активации"),
    ("Microphone volume", "Громкость микрофона"),
    ("Disable input device", "Отключить устройство ввода"),
    ("Output device", "Устройство вывода"),
    ("Voice chat volume", "Громкость голосового чата"),
    ("Blocks muffle voices", "Заглушение через блоки"),
    ("Source volume", "Громкость источников"),
    ("Player volume", "Громкость игроков"),
    ("No voice server here", "Здесь нет голосового сервера"),
    ("Activation type", "Тип активации"),
    ("Key toggles", "Кнопка переключения"),
    ("Distance", "Дистанция"),
    ("Enable overlay", "Включить оверлей"),
    ("Overlay position", "Расположение оверлея"),
    ("Who is speaking", "Кто говорит"),
    ("Entity icons", "Иконки энтити"),
    ("Marks over heads", "Значки над головами"),
    ("Microphone filters", "Фильтры микрофона"),
    ("Noise gate", "Шумовой гейт"),
    ("Noise gate level", "Порог шумового гейта"),
    ("Compressor", "Компрессор"),
    ("Limiter", "Лимитер"),
    ("Sound", "Звук"),
    ("Show icons", "Показывать иконки"),
    ("Activation icon", "Иконка активации"),
    ("Exponential distance falloff", "Экспоненциальное затухание"),
    ("Directional sound (left/right)", "Направленный звук (лево/право)"),
    ("General", "Основные"),
    ("Talk", "Говорить"),
    ("System default", "По умолчанию"),
    ("On", "Вкл"),
    ("Off", "Выкл"),
    ("None", "Нет"),
    ("Push to talk", "Нажать и говорить"),
    ("Voice", "Голос"),
    ("Voice activation", "Голосовая активация"),
    ("As proximity", "Как локальный"),
    ("Top left", "Слева сверху"),
    ("Top right", "Справа сверху"),
    ("Bottom left", "Слева снизу"),
    ("Bottom right", "Справа снизу"),
    ("Voice: connecting...", "Голос: подключение..."),
    ("Voice: no connection", "Голос: нет соединения"),
    ("Voice chat off", "Голосовой чат выключен"),
    ("Microphone muted", "Микрофон выключен"),
    ("Hold {} to talk", "Удерживайте {}, чтобы говорить"),
    ("Blocks muffle voices: on", "Заглушение через блоки: вкл"),
    ("Blocks muffle voices: off", "Заглушение через блоки: выкл"),
    ("Microphone on", "Микрофон включён"),
    ("Voice chat off: you hear nobody and send nothing", "Голосовой чат выключен: вы никого не слышите и ничего не передаёте"),
    ("Voice chat on", "Голосовой чат включён"),
    ("Voice chat: this server has no Plasmo Voice", "Голосовой чат: на этом сервере нет Plasmo Voice"),
    ("Push to talk (proximity)", "Говорить (локальный)"),
    ("Voice menu", "Меню голоса"),
    ("Mute microphone", "Выключить микрофон"),
    ("Voice chat on/off", "Вкл/выкл голосовой чат"),
    ("Proximity distance up", "Увеличить дистанцию"),
    ("Proximity distance down", "Уменьшить дистанцию"),
    ("Blocks muffle voices on/off", "Вкл/выкл заглушение через блоки"),
    ("Voice chat", "Голосовой чат"),
    ("Everything else: press {} in a world", "Всё остальное: нажмите {} в мире"),
    ("Voice chat: couldn't read the server's voice key", "Голосовой чат: не удалось прочитать ключ голосового сервера"),
    ("Voice debug: no voice server here", "Отладка голоса: здесь нет голосового сервера"),
];

thread_local! {
    static IN_RUSSIAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Follow the game's language (`ru_ru`).
fn set_language(game: &crate::game::Game<'_>) {
    IN_RUSSIAN.with(|r| r.set(game.settings.language.starts_with("ru")));
}

/// `s` in the game's language (Russian or English).
fn tr(s: &str) -> String {
    if IN_RUSSIAN.with(|r| r.get())
        && let Some((_, ru)) = RUSSIAN.iter().find(|(en, _)| *en == s)
    {
        return ru.to_string();
    }
    s.to_string()
}

const CORNERS: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];

thread_local! {
    // None until the server first speaks on the channel, and after leaving it.
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
    // The settings, read from their file once.
    static CONFIG: RefCell<Option<(Settings, Overlay)>> = const { RefCell::new(None) };
    // Entities sources hang on that no model shows (markers, displays: a plugin's jukebox),
    // and where the server last put them, copied by the packet hook from the game's tracked
    // entities.
    static TRACKED: RefCell<std::collections::HashMap<i32, Option<[f64; 3]>>> = RefCell::new(Default::default());
}

/// The entities whose place the packet hook should copy.
pub(super) fn tracked_wanted() -> Vec<i32> {
    TRACKED.with(|t| t.borrow().keys().copied().collect())
}

/// The packet hook's copy of where entity `id` is.
pub(super) fn track(id: i32, at: [f64; 3]) {
    TRACKED.with(|t| {
        t.borrow_mut().insert(id, Some(at));
    });
}

// The RSA key pair the server encrypts the voice key with: 2048 bits, as Plasmo Voice's own,
// made once per game on a thread of its own (a second or two).
static KEY: OnceLock<Arc<Key>> = OnceLock::new();
static KEY_STARTED: Once = Once::new();

struct Key(horizon_protocol::rsa::RsaPrivateKey);

impl voice::RsaKey for Key {
    fn public_der(&self) -> Vec<u8> {
        self.0.public_der()
    }
    fn decrypt(&self, data: &[u8]) -> Option<Vec<u8>> {
        self.0.decrypt(data).ok()
    }
}

fn start_key() {
    KEY_STARTED.call_once(|| {
        std::thread::spawn(|| {
            if let Ok(k) = horizon_protocol::rsa::RsaPrivateKey::generate(2048) {
                let _ = KEY.set(Arc::new(Key(k)));
            }
        });
    });
}

// Settings: one file, `key=value` lines, in the mod's folder.

fn uuid_hex(u: u128) -> String {
    format!("{u:032x}")
}

fn to_text(s: &Settings, o: &Overlay) -> String {
    let mut lines = vec![
        format!("deafened={}", s.deafened),
        format!("muted={}", s.microphone_muted),
        format!("threshold={}", s.threshold_db),
        format!("volume={}", s.volume),
        format!("microphone={}", s.microphone_gain),
        format!("input={}", s.input_device.clone().unwrap_or_default()),
        format!("output={}", s.output_device.clone().unwrap_or_default()),
        format!("noise_gate={}", s.noise_gate),
        format!("noise_gate_db={}", s.noise_gate_db),
        format!("compressor={}", s.compressor),
        format!("limiter={}", s.limiter),
        // Stored as its opposite: earlier versions kept "exponential=false" as the default.
        format!("linear_distance={}", !s.exponential_distance),
        format!("occlusion={}", s.occlusion),
        format!("panning={}", s.panning),
        format!("input_disabled={}", s.input_disabled),
        format!("hud={}", o.shown),
        format!("corner={}", o.corner),
        format!("nametags={}", o.nametags),
        format!("speakers={}", o.speakers),
    ];
    for (name, a) in &s.activations {
        lines.push(format!("activation.{name}.mode={}", a.mode.name()));
        lines.push(format!("activation.{name}.toggle={}", a.toggle));
        lines.push(format!("activation.{name}.distance={}", a.distance));
    }
    for (name, v) in &s.line_volumes {
        lines.push(format!("line.{name}={v}"));
    }
    for (id, v) in &s.player_volumes {
        lines.push(format!("player.{}={v}", uuid_hex(*id)));
    }
    for id in &s.muted_players {
        lines.push(format!("silenced.{}=true", uuid_hex(*id)));
    }
    lines.sort();
    lines.join("\n") + "\n"
}

fn from_text(text: &str) -> (Settings, Overlay) {
    let mut s = Settings::default();
    let mut o = Overlay::default();
    for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
        let b = v == "true";
        let f = v.parse::<f32>().ok();
        let device = (!v.is_empty()).then(|| v.to_string());
        match k {
            "deafened" => s.deafened = b,
            "muted" => s.microphone_muted = b,
            "threshold" => s.threshold_db = f.unwrap_or(s.threshold_db),
            "volume" => s.volume = f.unwrap_or(1.0),
            "microphone" => s.microphone_gain = f.unwrap_or(1.0),
            "input" => s.input_device = device,
            "output" => s.output_device = device,
            "noise_gate" => s.noise_gate = b,
            "noise_gate_db" => s.noise_gate_db = f.unwrap_or(s.noise_gate_db),
            "compressor" => s.compressor = b,
            "limiter" => s.limiter = b,
            "linear_distance" => s.exponential_distance = !b,
            "occlusion" => s.occlusion = b,
            "panning" => s.panning = b,
            "input_disabled" => s.input_disabled = b,
            "hud" => o.shown = b,
            "corner" => o.corner = v.parse().unwrap_or(3).min(3),
            "nametags" => o.nametags = b,
            "speakers" => o.speakers = b,
            _ => {
                if let Some(rest) = k.strip_prefix("activation.")
                    && let Some((name, field)) = rest.rsplit_once('.')
                {
                    let a = s.activations.entry(name.to_string()).or_insert_with(|| ActivationSettings {
                        mode: if name == voice::proto::PROXIMITY { Mode::PushToTalk } else { Mode::Inherit },
                        ..Default::default()
                    });
                    match field {
                        "mode" => a.mode = Mode::parse(v).unwrap_or(a.mode),
                        "toggle" => a.toggle = b,
                        "distance" => a.distance = v.parse().unwrap_or(0),
                        _ => {}
                    }
                } else if let Some(name) = k.strip_prefix("line.") {
                    s.line_volumes.insert(name.to_string(), f.unwrap_or(1.0));
                } else if let Some(id) = k.strip_prefix("player.").and_then(|h| u128::from_str_radix(h, 16).ok()) {
                    s.player_volumes.insert(id, f.unwrap_or(1.0));
                } else if let Some(id) = k.strip_prefix("silenced.").and_then(|h| u128::from_str_radix(h, 16).ok()) {
                    s.muted_players.insert(id);
                }
            }
        }
    }
    (s, o)
}

fn file(game: &crate::game::Game<'_>) -> std::path::PathBuf {
    game.native_data_dir(ID).join("settings.txt")
}

fn config(game: &crate::game::Game<'_>) -> (Settings, Overlay) {
    CONFIG.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| from_text(&std::fs::read_to_string(file(game)).unwrap_or_default()))
            .clone()
    })
}

pub(super) fn settings(game: &crate::game::Game<'_>) -> Settings {
    config(game).0
}

fn overlay(game: &crate::game::Game<'_>) -> Overlay {
    config(game).1
}

/// Keep `s` and `o`, and hand `s` to the engine.
fn save(game: &crate::game::Game<'_>, s: Settings, o: Overlay) {
    let path = file(game);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // "Hear yourself" is a test: it doesn't outlast the game.
    let _ = std::fs::write(&path, to_text(&Settings { monitor: false, ..s.clone() }, &o));
    CONFIG.with(|c| *c.borrow_mut() = Some((s.clone(), o)));
    with(|e| e.set_settings(s));
}

fn save_settings(game: &crate::game::Game<'_>, s: Settings) {
    let o = overlay(game);
    save(game, s, o);
}

/// Run `f` on the engine, if there is one.
pub(super) fn with<R>(f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    ENGINE.with(|e| e.borrow_mut().as_mut().map(f))
}

/// A key as the player reads it: `key.keyboard.left.alt` is LEFT ALT.
fn label(key: &str) -> String {
    if key == "key.keyboard.unknown" {
        return "not bound".into();
    }
    let name = key.strip_prefix("key.keyboard.").or_else(|| key.strip_prefix("key.mouse.")).unwrap_or(key);
    name.replace('.', " ").to_uppercase()
}

/// The key name of an activation: proximity's is the talk key.
fn activation_key(name: &str) -> String {
    if name == voice::proto::PROXIMITY { TALK.to_string() } else { format!("act.{name}") }
}

fn key_of(game: &crate::game::Game<'_>, name: &str) -> String {
    let default = match name {
        TALK => TALK_DEFAULT,
        MENU => MENU_DEFAULT,
        MUTE => MUTE_DEFAULT,
        DEAFEN => DEAFEN_DEFAULT,
        _ => "key.keyboard.unknown",
    };
    game.native_mod_key(ID, name, default)
}

/// The game server's host from `host:port` (`[v6]:port` too).
fn host(address: &str) -> String {
    if let Some(rest) = address.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_string();
    }
    match address.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => h.to_string(),
        _ => address.to_string(),
    }
}

/// `minecraft:register` for both channels: Paper delivers a plugin's messages only on the
/// channels a client registered, and the server asks for the player's info on them.
fn register(game: &crate::game::Game<'_>) {
    let data = [voice::proto::CHANNEL, voice::proto::FLAG_CHANNEL].join("\0").into_bytes();
    game.send(play::Serverbound::CustomPayload(CustomPayload {
        channel: "minecraft:register".into(),
        data,
    }));
}

/// Every packet the server sends in play, before the game handles it.
pub(super) fn packet(game: &mut crate::game::Game<'_>, packet: &play::Clientbound) {
    if !game.native_on(ID) {
        return;
    }
    match packet {
        play::Clientbound::Login(_) => {
            start_key();
            register(game);
        }
        play::Clientbound::CustomPayload(p) if p.channel == voice::proto::CHANNEL => {
            let made = ENGINE.with(|e| e.borrow().is_none());
            if made {
                let mut engine = Engine::new(settings(game), MINECRAFT);
                engine.set_language(&game.settings.language);
                ENGINE.with(|e| *e.borrow_mut() = Some(engine));
            }
            with(|e| {
                if !e.has_key()
                    && let Some(k) = KEY.get()
                {
                    e.set_key(k.clone());
                }
                e.receive(&p.data)
            });
            flush(game);
        }
        _ => {}
    }
}

/// Send the engine's payloads; show its messages and action bar.
fn flush(game: &mut crate::game::Game<'_>) {
    let (out, messages, bar) =
        with(|e| (e.take_outgoing(), e.take_messages(), e.take_action_bar())).unwrap_or_default();
    for data in out {
        game.send(play::Serverbound::CustomPayload(CustomPayload {
            channel: voice::proto::CHANNEL.into(),
            data,
        }));
    }
    for m in messages {
        game.native_chat(&tr(&m));
    }
    if let Some(json) = bar.and_then(|j| serde_json::from_str::<Value>(&j).ok()) {
        game.titles.set_overlay(horizon_client::scoreboard::comp_text(&json), false);
    }
}

/// Where a mob is, by its network id.
fn mob(game: &crate::game::Game<'_>, id: i32) -> Option<[f64; 3]> {
    let m = game.mobs.iter().find(|m| m.id as i32 == id && !m.removed)?;
    Some([m.position.x, m.position.y, m.position.z])
}

/// Where any entity is, by its network id: this player (a source on the player holding
/// something), a mob or another player, a dropped item, an armor stand or an item frame.
fn entity(game: &crate::game::Game<'_>, id: i32) -> Option<[f64; 3]> {
    if game.remote.as_ref().is_some_and(|r| r.entity_id == id) {
        return game.native_player().map(|p| [p.x, p.y + EYES, p.z]);
    }
    if let Some(at) = mob(game, id) {
        return Some(at);
    }
    if let Some(d) = game.dropped.iter().find(|d| d.id as i32 == id) {
        return Some([d.position.x, d.position.y, d.position.z]);
    }
    if let Some(a) = game.armor_stands.iter().find(|a| a.id as i32 == id) {
        return Some([a.position.x, a.position.y, a.position.z]);
    }
    if let Some(f) = game.item_frames.iter().find(|f| f.id as i32 == id) {
        return Some(f.pos.map(|v| v as f64 + 0.5));
    }
    // Any other entity: where the server put it, once the packet hook has copied it.
    TRACKED.with(|t| *t.borrow_mut().entry(id).or_insert(None))
}

/// The solid blocks a straight line from `a` to `b` passes through, up to five (the source's
/// and the listener's own blocks left out).
fn occluded(game: &crate::game::Game<'_>, a: [f64; 3], b: [f64; 3]) -> u32 {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let steps = (length * 2.0).ceil() as usize;
    let ends = [a, b].map(|p| p.map(|v| v.floor() as i32));
    let mut seen: Vec<[i32; 3]> = Vec::new();
    for i in 1..steps {
        let t = i as f64 / steps as f64;
        let p = [0, 1, 2].map(|k| (a[k] + d[k] * t).floor() as i32);
        if ends.contains(&p) || seen.contains(&p) {
            continue;
        }
        let f = game.res.facts().get(game.world.block(p[0], p[1], p[2]));
        if f.full_collision {
            seen.push(p);
            if seen.len() >= 5 {
                break;
            }
        }
    }
    seen.len() as u32
}

/// Once a tick in the window's game: the engine follows the server, the player and the
/// sources.
pub(super) fn tick(game: &mut crate::game::Game<'_>) {
    if !game.native_window() {
        return;
    }
    let address = game.remote.as_ref().map(|r| r.session.address.clone());
    let (Some(address), true) = (address, game.native_on(ID)) else {
        // Off a server, or turned off: the engine goes.
        ENGINE.with(|e| e.borrow_mut().take());
        return;
    };
    if !game.native_playing() || game.dialog_open() {
        with(|e| e.release_keys());
    }
    let language = game.settings.language.clone();
    set_language(game);
    let listener = game.native_player().map(|p| Listener { position: [p.x, p.y + EYES, p.z], yaw: p.yaw });
    let me = game.remote.as_ref().map(|r| r.profile());
    let key = KEY.get().cloned();
    let host = host(&address);
    let game_ref: &crate::game::Game<'_> = game;
    let mut locate = |l: Locator| -> Option<[f64; 3]> {
        match l {
            Locator::Player(uuid) if Some(uuid) == me => listener.map(|l| l.position),
            Locator::Player(uuid) => {
                let id = game_ref.remote.as_ref()?.player_entity(uuid)?;
                mob(game_ref, id as i32).map(|[x, y, z]| [x, y + EYES, z])
            }
            Locator::Entity(id) => entity(game_ref, id),
        }
    };
    let mut blocks = |a: [f64; 3], b: [f64; 3]| occluded(game_ref, a, b);
    let marks = with(|e| {
        if !e.has_key()
            && let Some(k) = key
        {
            e.set_key(k);
        }
        e.set_language(&language);
        e.tick(&host, listener, &mut locate, &mut blocks);
        let speaking: HashSet<String> = e.status().speaking.into_iter().map(|(n, _)| n).collect();
        let s = e.settings();
        let silent: HashSet<String> = e
            .players()
            .into_iter()
            .filter(|p| s.muted_players.contains(&p.id) || p.voice_disabled || p.microphone_muted || p.muted)
            .map(|p| p.name)
            .collect();
        (speaking, silent)
    });
    // Who the marks over heads are for: speaking, and silent (muted by this player, or with
    // their voice or microphone off).
    let (speaking, silent) = marks.unwrap_or_default();
    horizon_client_render::proximity_marks::set(overlay(game).nametags, speaking, silent);
    flush(game);
}

/// Every key: the menu, mute, deafen, and each activation's talk key (held, or toggled).
pub(super) fn key(game: &mut crate::game::Game<'_>, key: &str, pressed: bool) {
    if !game.native_on(ID) {
        return;
    }
    let playing = game.native_playing() && !game.dialog_open();
    let activations = with(|e| e.activations()).unwrap_or_default();
    for a in &activations {
        if key == key_of(game, &activation_key(&a.name)) {
            let s = settings(game);
            let toggle = s.activation(&a.name).toggle;
            with(|e| {
                if toggle {
                    if pressed && playing {
                        let on = !e.is_held(&a.name);
                        e.set_held(&a.name, on);
                    }
                } else {
                    e.set_held(&a.name, pressed && playing);
                }
            });
        }
    }
    if !pressed || !playing {
        return;
    }
    if key == key_of(game, MENU) {
        screen_open(game);
    } else if key == key_of(game, DISTANCE_UP) || key == key_of(game, DISTANCE_DOWN) {
        step_distance(game, key == key_of(game, DISTANCE_UP));
    } else if key == key_of(game, OCCLUSION) {
        let mut s = settings(game);
        s.occlusion = !s.occlusion;
        let on = s.occlusion;
        save_settings(game, s);
        game.native_chat(&tr(if on { "Blocks muffle voices: on" } else { "Blocks muffle voices: off" }));
    } else if key == key_of(game, MUTE) {
        let mut s = settings(game);
        s.microphone_muted = !s.microphone_muted;
        save_settings(game, s);
    } else if key == key_of(game, DEAFEN) {
        let mut s = settings(game);
        s.deafened = !s.deafened;
        save_settings(game, s);
    }
}

// The HUD, as Plasmo Voice draws it: the microphone's state over the hotbar (talking on an
// activation, muted by the server or by the player, voice off, or no connection), the
// distance a moment after it changes, and who is speaking in a corner.

pub(super) fn hud(
    game: &mut crate::game::Game<'_>,
    batch: &mut horizon_client::hud::HudBatch,
    corners: &mut horizon_client::extras_hud::Corners,
) {
    use horizon_client::extras::Corner;
    let o = overlay(game);
    if !game.native_on(ID) || game.native_player().is_none() {
        return;
    }
    let me = game.remote.as_ref().map(|r| r.profile());
    let Some((status, activations, distance, server_muted)) = with(|e| {
        let muted = me.and_then(|m| e.player(m)).is_some_and(|p| p.muted);
        (e.status(), e.activations(), e.shown_distance(), muted)
    }) else {
        return;
    };
    if status.connection == Connection::Off {
        return;
    }
    let s = settings(game);
    let (w, h) = (batch.width, batch.height);
    let icon = match status.connection {
        Connection::Off => None,
        Connection::Connecting | Connection::TimedOut => icons::icon("microphone_disconnected"),
        Connection::Connected if s.deafened => icons::icon("speaker_disabled"),
        Connection::Connected if server_muted => icons::icon("microphone_muted"),
        Connection::Connected if s.microphone_muted => icons::icon("microphone_disabled"),
        Connection::Connected => activations
            .iter()
            .find(|a| status.transmitting.contains(&a.name))
            .map(|a| voice::texture::icon(&a.icon).or_else(|| icons::icon("microphone")))
            .flatten(),
    };
    // Over the hotbar, above the hearts and food.
    let (cx, top) = ((w / 2.0).floor(), h - 59.0);
    if o.shown
        && let Some(icon) = icon
    {
        draw_icon(batch, &game.gui.atlas, icon, cx - 8.0, top, 16.0);
    }
    if let Some(d) = distance {
        let text = format!("{}: {d}", tr("Distance"));
        let tw = game.native_text_width(&text);
        game.native_text(batch, &text, [cx - tw / 2.0, top - 11.0], WHITE, true);
    }
    // The overlay: each player speaking, on their own strip.
    if !o.speakers || status.speaking.is_empty() {
        return;
    }
    let corner = [Corner::TopLeft, Corner::TopRight, Corner::BottomLeft, Corner::BottomRight][o.corner];
    if !game.native_corner_shown(corner) {
        return;
    }
    let rows: Vec<&String> = status.speaking.iter().take(8).map(|(name, _)| name).collect();
    let bw = rows.iter().map(|n| game.native_text_width(n)).fold(0.0, f32::max) + 16.0;
    let (x, y) = corners.place(corner, bw, rows.len() as f32 * 12.0);
    for (i, name) in rows.iter().enumerate() {
        let ry = y + i as f32 * 12.0;
        game.native_fill(batch, [x, ry, bw, 11.0], 0x7800_0000);
        if let Some(i) = icons::icon("speaker") {
            draw_icon(batch, &game.gui.atlas, i, x + 1.0, ry + 1.0, 9.0);
        }
        game.native_text(batch, name, [x + 13.0, ry + 2.0], WHITE, true);
    }
}

// The menu: dialogs as a server sends them (Horizon draws their sliders, switches and lists).
// Every button runs `/vc apply <page> key=value… then=<page>` with the inputs' values; the
// `send_chat` hook hands it here before it would reach the server.

fn slider(key: &str, label: &str, start: f32, end: f32, step: f32, initial: f32) -> Value {
    json!({
        "type": "minecraft:number_range", "key": key, "label": label, "width": 300,
        "start": start, "end": end, "step": step, "initial": initial.clamp(start, end),
    })
}

fn switch(key: &str, label: &str, initial: bool) -> Value {
    json!({ "type": "minecraft:boolean", "key": key, "label": label, "initial": initial })
}

fn choice(key: &str, label: &str, options: &[(String, String)], initial: &str) -> Value {
    let options: Vec<Value> = options
        .iter()
        .map(|(id, display)| json!({ "id": id, "display": display, "initial": id == initial }))
        .collect();
    json!({ "type": "minecraft:single_option", "key": key, "label": label, "width": 300, "options": options })
}

fn message(text: &str) -> Value {
    json!({ "type": "minecraft:plain_message", "contents": text, "width": 300 })
}

/// A button that applies `page`'s inputs, then opens `then` (or closes).
fn button(label: &str, page: &str, inputs: &[Value], then: &str, width: i32) -> Value {
    let mut template = format!("vc apply {page}");
    for i in inputs {
        if let Some(k) = i.get("key").and_then(Value::as_str) {
            template.push_str(&format!(" {k}=$({k})"));
        }
    }
    template.push_str(&format!(" then={then}"));
    json!({ "label": label, "width": width, "action": { "type": "minecraft:dynamic/run_command", "template": template } })
}

const PAGES: [(&str, &str); 6] = [
    ("devices", "Devices"),
    ("activations", "Activations"),
    ("volume", "Volume"),
    ("players", "Players"),
    ("advanced", "Advanced"),
    ("overlay", "Overlay"),
];

fn dialog(title: &str, page: &str, body: Vec<Value>, inputs: Vec<Value>, mut actions: Vec<Value>) -> Value {
    if page != "main" {
        actions.push(button("< Back", page, &inputs, "main", 100));
    }
    let exit = button("Done", page, &inputs, "close", 100);
    json!({
        "type": "minecraft:multi_action",
        "title": format!("Proximity Voice: {title}"),
        "can_close_with_escape": true,
        "pause": false,
        "after_action": "close",
        "body": body,
        "inputs": inputs,
        "actions": actions,
        "exit_action": exit,
        "columns": 3,
    })
}

fn device_options(names: Vec<String>) -> Vec<(String, String)> {
    let mut out = vec![("0".to_string(), "System default".to_string())];
    out.extend(names.into_iter().enumerate().map(|(i, n)| ((i + 1).to_string(), n)));
    out
}

fn device_id(names: &[String], chosen: &Option<String>) -> String {
    chosen
        .as_ref()
        .and_then(|c| names.iter().position(|n| n == c))
        .map_or("0".into(), |i| (i + 1).to_string())
}

/// The menu's page `page` (`player.<uuid>` for one player).
fn page_dialog(game: &crate::game::Game<'_>, page: &str) -> Value {
    let s = settings(game);
    let o = overlay(game);
    let status = with(|e| e.status());
    let state = match status.as_ref().map(|s| s.connection) {
        None | Some(Connection::Off) => "This server has no Plasmo Voice.",
        Some(Connection::Connecting) => "Connecting to the voice server...",
        Some(Connection::TimedOut) => "The voice server stopped answering.",
        Some(Connection::Connected) => "Connected.",
    };
    match page {
        "devices" => {
            let (inputs_list, outputs_list) = (Engine::input_devices(), Engine::output_devices());
            let inputs = vec![
                choice("input", "Microphone", &device_options(inputs_list.clone()), &device_id(&inputs_list, &s.input_device)),
                choice("output", "Speakers", &device_options(outputs_list.clone()), &device_id(&outputs_list, &s.output_device)),
                slider("volume", "Volume", 0.0, 200.0, 5.0, s.volume * 100.0),
                slider("microphone", "Microphone volume", 0.0, 200.0, 5.0, s.microphone_gain * 100.0),
                switch("monitor", "Hear yourself (microphone test)", s.monitor),
            ];
            let body = vec![message(&format!(
                "Microphone: {}\nSpeakers: {}",
                status.as_ref().and_then(|s| s.microphone.clone()).unwrap_or_else(|| "not open".into()),
                status.as_ref().and_then(|s| s.speakers.clone()).unwrap_or_else(|| "not open".into()),
            ))];
            dialog("Devices", page, body, inputs, Vec::new())
        }
        "activations" => {
            let activations = with(|e| e.activations()).unwrap_or_default();
            let mut inputs = vec![slider("threshold", "Voice activation level (dB)", -60.0, 0.0, 1.0, s.threshold_db)];
            let mut keys = Vec::new();
            for (i, a) in activations.iter().enumerate() {
                let mine = s.activation(&a.name);
                let mut modes = vec![("ptt".to_string(), "Push to talk".to_string()), ("voice".to_string(), "Voice activation".to_string())];
                if !a.proximity {
                    modes.insert(0, ("inherit".to_string(), "As proximity".to_string()));
                }
                let mode = if a.proximity && mine.mode == Mode::Inherit { Mode::PushToTalk } else { mine.mode };
                inputs.push(choice(&format!("mode{i}"), &a.label, &modes, mode.name()));
                inputs.push(switch(&format!("toggle{i}"), &format!("{}: key toggles", a.label), mine.toggle));
                match a.distances.as_slice() {
                    [] => {}
                    [-1, max] => inputs.push(slider(&format!("distance{i}"), &format!("{}: distance", a.label), 1.0, *max as f32, 1.0, a.distance as f32)),
                    list => {
                        let options: Vec<(String, String)> = list.iter().map(|d| (d.to_string(), d.to_string())).collect();
                        inputs.push(choice(&format!("distance{i}"), &format!("{}: distance", a.label), &options, &a.distance.to_string()));
                    }
                }
                keys.push(format!("{}: {}", a.label, label(&key_of(game, &activation_key(&a.name)))));
            }
            let body = vec![message(&format!(
                "Keys (Options > Mods > Proximity Voice): {}. Menu: {}, mute: {}, voice chat off: {}.",
                if keys.is_empty() { "none yet".into() } else { keys.join(", ") },
                label(&key_of(game, MENU)),
                label(&key_of(game, MUTE)),
                label(&key_of(game, DEAFEN)),
            ))];
            dialog("Activations", page, body, inputs, Vec::new())
        }
        "volume" => {
            let lines = with(|e| e.lines()).unwrap_or_default();
            let mut inputs = vec![slider("volume", "Volume", 0.0, 200.0, 5.0, s.volume * 100.0)];
            for (i, l) in lines.iter().enumerate() {
                let v = s.line_volumes.get(&l.name).copied().unwrap_or(l.default_volume);
                inputs.push(slider(&format!("line{i}"), &l.label, 0.0, 200.0, 5.0, v * 100.0));
            }
            dialog("Volume", page, vec![message("Each kind of voice the server sends has its own volume.")], inputs, Vec::new())
        }
        "players" => {
            let players = with(|e| e.players()).unwrap_or_default();
            let me = game.remote.as_ref().map(|r| r.profile());
            let actions: Vec<Value> = players
                .iter()
                .filter(|p| Some(p.id) != me)
                .take(60)
                .map(|p| {
                    let state = if s.muted_players.contains(&p.id) {
                        " (muted)"
                    } else if p.voice_disabled || p.microphone_muted {
                        " (mic off)"
                    } else {
                        ""
                    };
                    button(&format!("{}{state}", p.name), page, &[], &format!("player.{}", uuid_hex(p.id)), 100)
                })
                .collect();
            let body = vec![message(if actions.is_empty() { "Nobody else here has voice chat." } else { "A player's volume, or mute them for yourself." })];
            dialog("Players", page, body, Vec::new(), actions)
        }
        p if p.starts_with("player.") => {
            let id = u128::from_str_radix(&p["player.".len()..], 16).unwrap_or(0);
            let name = with(|e| e.player(id).map(|p| p.name.clone())).flatten().unwrap_or_else(|| "?".into());
            let inputs = vec![
                slider("volume", &format!("{name}'s volume"), 0.0, 200.0, 5.0, s.player_volumes.get(&id).copied().unwrap_or(1.0) * 100.0),
                switch("muted", &format!("Mute {name}"), s.muted_players.contains(&id)),
            ];
            let mut d = dialog(&name, page, Vec::new(), inputs.clone(), Vec::new());
            // Back goes to the list.
            d["actions"] = json!([button("< Players", page, &inputs, "players", 100)]);
            d
        }
        "advanced" => {
            let inputs = vec![
                switch("noise_gate", "Noise gate", s.noise_gate),
                slider("noise_gate_db", "Noise gate level (dB)", -80.0, -20.0, 1.0, s.noise_gate_db),
                switch("compressor", "Compressor", s.compressor),
                switch("limiter", "Limiter", s.limiter),
                switch("exponential", "Exponential distance falloff", s.exponential_distance),
                switch("occlusion", "Blocks muffle voices", s.occlusion),
            ];
            dialog("Advanced", page, vec![message("Microphone filters, in this order, and how voices travel.")], inputs, Vec::new())
        }
        "overlay" => {
            let corners: Vec<(String, String)> = CORNERS.iter().enumerate().map(|(i, c)| (i.to_string(), c.to_string())).collect();
            let inputs = vec![
                switch("hud", "Voice box on the screen", o.shown),
                choice("corner", "Box corner", &corners, &o.corner.to_string()),
                switch("speakers", "Who is speaking, in the box", o.speakers),
                switch("nametags", "Marks over players' heads", o.nametags),
            ];
            dialog("Overlay", page, Vec::new(), inputs, Vec::new())
        }
        _ => {
            let inputs = vec![
                switch("enabled", "Voice chat", !s.deafened),
                switch("microphone", "Microphone", !s.microphone_muted),
            ];
            let actions: Vec<Value> = PAGES.iter().map(|(id, title)| button(title, "main", &inputs, id, 100)).collect();
            dialog("Settings", "main", vec![message(state)], inputs, actions)
        }
    }
}

fn open(game: &mut crate::game::Game<'_>, page: &str) {
    let tag = horizon_client::scoreboard::json_to_tag(&page_dialog(game, page));
    game.show_dialog(&tag);
}

/// `/vc apply <page> key=value…`: the page's inputs into the settings.
fn apply(game: &mut crate::game::Game<'_>, page: &str, values: &[(&str, &str)]) {
    let mut s = settings(game);
    let mut o = overlay(game);
    let get = |k: &str| values.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
    let flag = |k: &str| get(k).map(|v| v == "true");
    let num = |k: &str| get(k).and_then(|v| v.parse::<f32>().ok());
    match page {
        "main" => {
            if let Some(b) = flag("enabled") {
                s.deafened = !b;
            }
            if let Some(b) = flag("microphone") {
                s.microphone_muted = !b;
            }
        }
        "devices" => {
            let pick = |names: Vec<String>, v: Option<&str>| -> Option<Option<String>> {
                let i: usize = v?.parse().ok()?;
                Some(if i == 0 { None } else { names.get(i - 1).cloned() })
            };
            if let Some(d) = pick(Engine::input_devices(), get("input")) {
                s.input_device = d;
            }
            if let Some(d) = pick(Engine::output_devices(), get("output")) {
                s.output_device = d;
            }
            if let Some(v) = num("volume") {
                s.volume = v / 100.0;
            }
            if let Some(v) = num("microphone") {
                s.microphone_gain = v / 100.0;
            }
            if let Some(b) = flag("monitor") {
                s.monitor = b;
            }
        }
        "activations" => {
            if let Some(v) = num("threshold") {
                s.threshold_db = v;
            }
            let activations = with(|e| e.activations()).unwrap_or_default();
            let mut shown = None;
            for (i, a) in activations.iter().enumerate() {
                let mut mine = s.activation(&a.name);
                if let Some(m) = get(&format!("mode{i}")).and_then(Mode::parse) {
                    mine.mode = m;
                }
                if let Some(b) = flag(&format!("toggle{i}")) {
                    mine.toggle = b;
                }
                if let Some(d) = num(&format!("distance{i}")) {
                    let d = d as i32;
                    if d != a.distance {
                        shown = Some(d);
                    }
                    mine.distance = d;
                }
                s.activations.insert(a.name.clone(), mine);
            }
            if let Some(d) = shown {
                with(|e| e.show_distance(d));
            }
        }
        "volume" => {
            if let Some(v) = num("volume") {
                s.volume = v / 100.0;
            }
            let lines = with(|e| e.lines()).unwrap_or_default();
            for (i, l) in lines.iter().enumerate() {
                if let Some(v) = num(&format!("line{i}")) {
                    s.line_volumes.insert(l.name.clone(), v / 100.0);
                }
            }
        }
        p if p.starts_with("player.") => {
            if let Ok(id) = u128::from_str_radix(&p["player.".len()..], 16) {
                if let Some(v) = num("volume") {
                    s.player_volumes.insert(id, v / 100.0);
                }
                match flag("muted") {
                    Some(true) => {
                        s.muted_players.insert(id);
                    }
                    Some(false) => {
                        s.muted_players.remove(&id);
                    }
                    None => {}
                }
            }
        }
        "advanced" => {
            for (k, field) in [
                ("noise_gate", &mut s.noise_gate),
                ("compressor", &mut s.compressor),
                ("limiter", &mut s.limiter),
                ("exponential", &mut s.exponential_distance),
                ("occlusion", &mut s.occlusion),
            ] {
                if let Some(b) = flag(k) {
                    *field = b;
                }
            }
            if let Some(v) = num("noise_gate_db") {
                s.noise_gate_db = v;
            }
        }
        "overlay" => {
            if let Some(b) = flag("hud") {
                o.shown = b;
            }
            if let Some(c) = get("corner").and_then(|c| c.parse::<usize>().ok()) {
                o.corner = c.min(3);
            }
            if let Some(b) = flag("speakers") {
                o.speakers = b;
            }
            if let Some(b) = flag("nametags") {
                o.nametags = b;
            }
        }
        _ => {}
    }
    save(game, s, o);
}

/// A command of the menu's buttons on its way to the server: run here instead.
pub(super) fn chat(game: &mut crate::game::Game<'_>, message: &str) -> bool {
    let Some(command) = message.trim().strip_prefix('/') else { return false };
    let mut ran = false;
    self::command(game, command, &mut ran);
    ran
}

/// `/vc`: the state; `/vc menu`; and the settings by name.
pub(super) fn command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    let Some(args) = command.strip_prefix("vc") else { return };
    if !game.native_on(ID) || !(args.is_empty() || args.starts_with(' ')) {
        return;
    }
    *ran = true;
    let words: Vec<&str> = args.split_whitespace().collect();
    let mut s = settings(game);
    let reply = match words.as_slice() {
        [] => status_line(game),
        ["menu"] => {
            screen_open(game);
            return;
        }
        ["ui"] => {
            open(game, "main");
            return;
        }
        ["ui", page] => {
            open(game, page);
            return;
        }
        ["apply", page, rest @ ..] => {
            let values: Vec<(&str, &str)> = rest.iter().filter_map(|w| w.split_once('=')).collect();
            apply(game, page, &values);
            if let Some(then) = values.iter().find(|(k, _)| *k == "then").map(|(_, v)| *v)
                && then != "close"
            {
                open(game, then);
            }
            return;
        }
        ["debug"] => {
            debug(game);
            return;
        }
        ["mute"] => {
            s.microphone_muted = true;
            "Microphone muted".into()
        }
        ["unmute"] => {
            s.microphone_muted = false;
            "Microphone on".into()
        }
        ["deafen"] => {
            s.deafened = true;
            "Voice chat off: you hear nobody and send nothing".into()
        }
        ["undeafen"] => {
            s.deafened = false;
            "Voice chat on".into()
        }
        ["mute", name] | ["unmute", name] => {
            let found = with(|e| e.players().into_iter().find(|p| p.name.eq_ignore_ascii_case(name))).flatten();
            match found {
                Some(p) => {
                    if words[0] == "mute" {
                        s.muted_players.insert(p.id);
                        format!("{} muted for you", p.name)
                    } else {
                        s.muted_players.remove(&p.id);
                        format!("{} unmuted", p.name)
                    }
                }
                None => format!("No player {name} with voice chat"),
            }
        }
        _ => "/vc [menu|mute|unmute|deafen|undeafen|mute <player>|unmute <player>]".into(),
    };
    save_settings(game, s);
    game.native_chat(&tr(&reply));
}

/// `/vc debug`: where each voice heard was placed, and how each player with voice chat is
/// found in the world (profile, entity, model).
fn debug(game: &mut crate::game::Game<'_>) {
    let Some((ticks, placed, players)) = with(|e| {
        let (ticks, placed) = e.debug();
        (ticks, placed, e.players())
    }) else {
        game.native_chat(&tr("Voice debug: no voice server here"));
        return;
    };
    let me = game.remote.as_ref().map(|r| r.profile());
    let listener = game.native_player().map(|p| format!("{:.1} {:.1} {:.1} yaw {:.0}", p.x, p.y, p.z, p.yaw));
    let mut lines = vec![format!(
        "Voice debug: {ticks} ticks, you {} at {}",
        me.map(uuid_hex).unwrap_or_else(|| "?".into()),
        listener.unwrap_or_else(|| "nowhere".into())
    )];
    if placed.is_empty() {
        lines.push("Nobody heard in the last 2 s".into());
    }
    lines.extend(placed);
    for p in players.iter().filter(|p| Some(p.id) != me) {
        let entity = game.remote.as_ref().and_then(|r| r.player_entity(p.id));
        let model = entity.and_then(|id| mob(game, id as i32));
        lines.push(format!(
            "{} {}: entity {}, model {}",
            p.name,
            uuid_hex(p.id),
            entity.map_or("none".into(), |e| e.to_string()),
            model.map_or("none".into(), |[x, y, z]| format!("at {x:.1} {y:.1} {z:.1}")),
        ));
    }
    for l in lines {
        game.native_chat(&l);
    }
}

fn status_line(game: &crate::game::Game<'_>) -> String {
    let Some(status) = with(|e| e.status()) else {
        return "Voice chat: this server has no Plasmo Voice".into();
    };
    let state = match status.connection {
        Connection::Off => return "Voice chat: this server has no Plasmo Voice".into(),
        Connection::Connecting => "connecting",
        Connection::TimedOut => "no connection",
        Connection::Connected => "connected",
    };
    let s = settings(game);
    format!(
        "Voice chat: {state}; mic {} ({:.0} dB); speakers {}{}{}. Menu: {}",
        status.microphone.as_deref().unwrap_or("none"),
        status.microphone_db,
        status.speakers.as_deref().unwrap_or("none"),
        if s.microphone_muted { "; muted" } else { "" },
        if s.deafened { "; voice chat off" } else { "" },
        label(&key_of(game, MENU)),
    )
}

/// The mod's keys for Options > Mods: the fixed ones, then one per activation the server
/// offers beyond proximity.
pub(super) fn keys() -> Vec<[String; 3]> {
    let mut keys = vec![
        [TALK.into(), tr("Push to talk (proximity)"), TALK_DEFAULT.into()],
        [MENU.into(), tr("Voice menu"), MENU_DEFAULT.into()],
        [MUTE.into(), tr("Mute microphone"), MUTE_DEFAULT.into()],
        [DEAFEN.into(), tr("Voice chat on/off"), DEAFEN_DEFAULT.into()],
        [DISTANCE_UP.into(), tr("Proximity distance up"), "key.keyboard.unknown".into()],
        [DISTANCE_DOWN.into(), tr("Proximity distance down"), "key.keyboard.unknown".into()],
        [OCCLUSION.into(), tr("Blocks muffle voices on/off"), "key.keyboard.unknown".into()],
    ];
    for a in with(|e| e.activations()).unwrap_or_default().iter().filter(|a| !a.proximity) {
        keys.push([activation_key(&a.name), format!("{}: {}", tr("Talk"), a.label), "key.keyboard.unknown".into()]);
    }
    keys
}

/// The rows of the settings page in Options > Mods.
pub(super) fn page(game: &crate::game::Game<'_>) -> Vec<String> {
    let s = settings(game);
    set_language(game);
    let on = |b: bool| tr(on_off(b));
    vec![
        format!("{}: {}", tr("Voice chat"), on(!s.deafened)),
        format!("{}: {}", tr("Microphone"), on(!s.microphone_muted)),
        tr("Everything else: press {} in a world").replace("{}", &label(&key_of(game, MENU))),
    ]
}

pub(super) fn click(game: &mut crate::game::Game<'_>, button: usize) {
    let mut s = settings(game);
    match button {
        0 => s.deafened = !s.deafened,
        1 => s.microphone_muted = !s.microphone_muted,
        _ => return,
    }
    save_settings(game, s);
}

/// The distance keys: proximity's next allowed distance up or down.
fn step_distance(game: &mut crate::game::Game<'_>, up: bool) {
    let Some(a) = with(|e| e.activations().into_iter().find(|a| a.proximity)).flatten() else { return };
    let options: Vec<i32> = match a.distances.as_slice() {
        [] => return,
        [-1, max] => (1..=*max).collect(),
        list => list.to_vec(),
    };
    let i = options.iter().position(|d| *d == a.distance).unwrap_or(0);
    let next = if up { (i + 1).min(options.len() - 1) } else { i.saturating_sub(1) };
    let d = options[next];
    let mut s = settings(game);
    let mut mine = s.activation(&a.name);
    mine.distance = d;
    s.activations.insert(a.name.clone(), mine);
    save_settings(game, s);
    with(|e| e.show_distance(d));
}

// The settings screen, drawn and driven as Plasmo Voice's: tabs along the top, a row per
// setting (its label, its control, a reset button), sliders, switches, lists and key buttons
// in Horizon's own widgets, and Plasmo Voice's icons. It lives in the dialog screen's place:
// while it is open the game takes it for an open dialog and routes the frame and the input
// to it (the hooks in dialogs.rs).

use horizon_client::hud::{HudBatch, rgba};
use horizon_client::text::Text;
use horizon_client::ui;
use proximity_voice_engine::icons::{self, Icon};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Devices,
    Volume,
    Activations,
    Overlay,
    Advanced,
    Keys,
}

const TABS: [(Tab, &str, &str); 6] = [
    (Tab::Devices, "Devices", "tabs/devices"),
    (Tab::Volume, "Volume", "tabs/volume"),
    (Tab::Activations, "Activations", "tabs/activation"),
    (Tab::Overlay, "Overlay", "tabs/overlay"),
    (Tab::Advanced, "Advanced", "tabs/advanced"),
    (Tab::Keys, "Keys", "tabs/hotkeys"),
];

/// What a row sets.
#[derive(Clone, Debug, PartialEq)]
enum Id {
    Threshold,
    Input,
    MicrophoneGain,
    InputDisabled,
    Output,
    Volume,
    NoiseGate,
    NoiseGateDb,
    Compressor,
    Limiter,
    Exponential,
    Occlusion,
    Panning,
    Line(String),
    Player(u128),
    Mode(String),
    Toggle(String),
    Distance(String),
    Hud,
    Corner,
    Speakers,
    Nametags,
    Key(String),
}

/// A row's control.
#[derive(Clone, Debug, PartialEq)]
enum Control {
    /// A slider at 0..1, with its text.
    Slider(f32, String),
    Switch(bool),
    /// A list: its options and the chosen one.
    Choice(Vec<String>, usize),
    /// A key: its name (`key.keyboard.v`).
    Key(String),
}

/// A button beside the control: a mute switch, or the microphone test.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Mute(bool),
    Test(bool),
}

#[derive(Clone)]
enum Item {
    Header(String),
    Row { id: Id, icon: Option<&'static Icon>, label: String, control: Control, side: Option<Side> },
    Search,
}

struct ScreenState {
    tab: Tab,
    scroll: f32,
    dragging: Option<Id>,
    /// An open list: its row, options and the rectangle under its control.
    choice: Option<(Id, Vec<String>, [f32; 4])>,
    /// A key button waiting for a key.
    binding: Option<String>,
    search: ui::EditBox,
    searching: bool,
}

thread_local! {
    static SCREEN: RefCell<Option<ScreenState>> = const { RefCell::new(None) };
}

const ROW: f32 = 24.0;
const HEADER: f32 = 20.0;
const TOP: f32 = 36.0;

fn percent(v: f32) -> (f32, String) {
    ((v / 2.0).clamp(0.0, 1.0), format!("{:.0}%", v * 100.0))
}

fn on_off(b: bool) -> &'static str {
    if b { "On" } else { "Off" }
}

fn row(id: Id, icon: Option<&'static Icon>, label: &str, control: Control, side: Option<Side>) -> Item {
    Item::Row { id, icon, label: tr(label), control, side }
}

fn device_choice(names: Vec<String>, chosen: &Option<String>) -> Control {
    let mut options = vec!["System default".to_string()];
    let index = chosen.as_ref().and_then(|c| names.iter().position(|n| n == c)).map_or(0, |i| i + 1);
    options.extend(names);
    Control::Choice(options, index)
}

/// The rows of `tab`, from the settings and what the server offers.
fn items(game: &crate::game::Game<'_>, tab: Tab, search: &str) -> Vec<Item> {
    let s = settings(game);
    let o = overlay(game);
    let mut out = Vec::new();
    match tab {
        Tab::Devices => {
            out.push(Item::Header("Microphone".into()));
            let t = ((s.threshold_db + 60.0) / 60.0).clamp(0.0, 1.0);
            out.push(row(Id::Threshold, None, "Activation threshold", Control::Slider(t, format!("{:.0} dB", s.threshold_db)), Some(Side::Test(s.monitor))));
            out.push(row(Id::Input, None, "Microphone", device_choice(Engine::input_devices(), &s.input_device), None));
            let (v, t) = percent(s.microphone_gain);
            out.push(row(Id::MicrophoneGain, None, "Microphone volume", Control::Slider(v, t), None));
            out.push(row(Id::InputDisabled, None, "Disable input device", Control::Switch(s.input_disabled), None));
            out.push(Item::Header("Output device".into()));
            out.push(row(Id::Output, None, "Output device", device_choice(Engine::output_devices(), &s.output_device), None));
            let (v, t) = percent(s.volume);
            out.push(row(Id::Volume, None, "Voice chat volume", Control::Slider(v, t), None));
            out.push(row(Id::Occlusion, None, "Blocks muffle voices", Control::Switch(s.occlusion), None));
        }
        Tab::Volume => {
            out.push(Item::Header("Source volume".into()));
            for l in with(|e| e.lines()).unwrap_or_default() {
                let v = s.line_volumes.get(&l.name).copied().unwrap_or(l.default_volume);
                let (p, t) = percent(v);
                let icon = voice::texture::icon(&l.icon).or_else(|| icons::icon("speaker"));
                out.push(row(Id::Line(l.name.clone()), icon, &l.label, Control::Slider(p, t), Some(Side::Mute(v == 0.0))));
            }
            out.push(Item::Header("Player volume".into()));
            out.push(Item::Search);
            let me = game.remote.as_ref().map(|r| r.profile());
            let search = search.to_lowercase();
            for p in with(|e| e.players()).unwrap_or_default() {
                if Some(p.id) == me || !p.name.to_lowercase().contains(&search) {
                    continue;
                }
                let v = s.player_volumes.get(&p.id).copied().unwrap_or(1.0);
                let (pv, t) = percent(v);
                let icon = if p.voice_disabled {
                    icons::icon("headset_disabled")
                } else if p.microphone_muted || p.muted {
                    icons::icon("microphone_muted")
                } else {
                    icons::icon("speaker")
                };
                out.push(row(Id::Player(p.id), icon, &p.name, Control::Slider(pv, t), Some(Side::Mute(s.muted_players.contains(&p.id)))));
            }
        }
        Tab::Activations => {
            let activations = with(|e| e.activations()).unwrap_or_default();
            if activations.is_empty() {
                out.push(Item::Header("No voice server here".into()));
            }
            for a in activations {
                out.push(Item::Header(a.label.clone()));
                let mine = s.activation(&a.name);
                let icon = voice::texture::icon(&a.icon).or_else(|| icons::icon("microphone"));
                let (options, index) = if a.proximity {
                    (vec!["Push to talk".to_string(), "Voice".to_string()], usize::from(mine.mode == Mode::Voice))
                } else {
                    let index = match mine.mode {
                        Mode::Inherit => 0,
                        Mode::PushToTalk => 1,
                        Mode::Voice => 2,
                    };
                    (vec!["As proximity".to_string(), "Push to talk".to_string(), "Voice".to_string()], index)
                };
                out.push(row(Id::Mode(a.name.clone()), icon, "Activation type", Control::Choice(options, index), None));
                out.push(row(Id::Toggle(a.name.clone()), None, "Key toggles", Control::Switch(mine.toggle), None));
                match a.distances.as_slice() {
                    [] => {}
                    [-1, max] => {
                        let v = (a.distance - 1) as f32 / (*max - 1).max(1) as f32;
                        out.push(row(Id::Distance(a.name.clone()), None, &format!("{} \"{}\"", tr("Distance"), a.label), Control::Slider(v, a.distance.to_string()), None));
                    }
                    list => {
                        let options: Vec<String> = list.iter().map(|d| d.to_string()).collect();
                        let index = list.iter().position(|d| *d == a.distance).unwrap_or(0);
                        out.push(row(Id::Distance(a.name.clone()), None, &format!("{} \"{}\"", tr("Distance"), a.label), Control::Choice(options, index), None));
                    }
                }
            }
        }
        Tab::Overlay => {
            out.push(Item::Header("Activation icon".into()));
            out.push(row(Id::Hud, None, "Show icons", Control::Switch(o.shown), None));
            out.push(Item::Header("Overlay".into()));
            out.push(row(Id::Corner, None, "Overlay position", Control::Choice(CORNERS.iter().map(|c| c.to_string()).collect(), o.corner), None));
            out.push(row(Id::Speakers, None, "Enable overlay", Control::Switch(o.speakers), None));
            out.push(Item::Header("Entity icons".into()));
            out.push(row(Id::Nametags, None, "Marks over heads", Control::Switch(o.nametags), None));
        }
        Tab::Advanced => {
            out.push(Item::Header("Microphone filters".into()));
            out.push(row(Id::NoiseGate, None, "Noise gate", Control::Switch(s.noise_gate), None));
            let g = ((s.noise_gate_db + 80.0) / 60.0).clamp(0.0, 1.0);
            out.push(row(Id::NoiseGateDb, None, "Noise gate level", Control::Slider(g, format!("{:.0} dB", s.noise_gate_db)), None));
            out.push(row(Id::Compressor, None, "Compressor", Control::Switch(s.compressor), None));
            out.push(row(Id::Limiter, None, "Limiter", Control::Switch(s.limiter), None));
            out.push(Item::Header("Sound".into()));
            out.push(row(Id::Exponential, None, "Exponential distance falloff", Control::Switch(s.exponential_distance), None));
            out.push(row(Id::Panning, None, "Directional sound (left/right)", Control::Switch(s.panning), None));
        }
        Tab::Keys => {
            out.push(Item::Header("General".into()));
            for [name, label, _] in keys() {
                if name.starts_with("act.") || name == TALK {
                    continue;
                }
                let key = key_of(game, &name);
                out.push(row(Id::Key(name), None, &label, Control::Key(key), None));
            }
            out.push(Item::Header("Activations".into()));
            out.push(row(Id::Key(TALK.into()), icons::icon("microphone"), "Proximity: push to talk", Control::Key(key_of(game, TALK)), None));
            for a in with(|e| e.activations()).unwrap_or_default().iter().filter(|a| !a.proximity) {
                let name = activation_key(&a.name);
                let key = key_of(game, &name);
                out.push(row(Id::Key(name), voice::texture::icon(&a.icon), &format!("{}: {}", a.label, tr("Talk")), Control::Key(key), None));
            }
        }
    }
    out
}

/// The column's x positions on a screen `w` wide.
struct Columns {
    label: f32,
    control: f32,
    width: f32,
    side: f32,
    reset: f32,
}

fn columns(w: f32) -> Columns {
    let c = (w / 2.0).floor();
    Columns { label: c - 180.0, control: c + 5.0, width: 150.0, side: c + 135.0, reset: c + 160.0 }
}

/// Each item with its top y (scrolled), and the content's height.
fn placed(items: Vec<Item>, scroll: f32) -> (Vec<(Item, f32)>, f32) {
    let start = TOP + 6.0 - scroll;
    let mut y = start;
    let mut out = Vec::new();
    for item in items {
        let h = if matches!(item, Item::Header(_)) { HEADER } else { ROW };
        out.push((item, y));
        y += h;
    }
    (out, y - start)
}

/// A control's width: narrower when a side button sits beside it.
fn control_width(c: &Columns, side: &Option<Side>) -> f32 {
    if side.is_some() { c.width - 25.0 } else { c.width }
}

fn draw_icon(batch: &mut HudBatch, atlas: &horizon_client::hud::HudAtlas, icon: &Icon, x: f32, y: f32, size: f32) {
    let px = size / icon.size as f32;
    for (i, p) in icon.pixels.iter().enumerate() {
        let a = (p >> 24) as u8;
        if a == 0 {
            continue;
        }
        let (cx, cy) = ((i % icon.size) as f32, (i / icon.size) as f32);
        batch.fill(atlas, x + cx * px, y + cy * px, px, px, rgba((p >> 16) as u8, (p >> 8) as u8, *p as u8, a));
    }
}

fn icon_button(
    batch: &mut HudBatch,
    atlas: &horizon_client::hud::HudAtlas,
    cursor: (f32, f32),
    icon: Option<&Icon>,
    x: f32,
    y: f32,
) {
    let hover = ui::hovered(cursor, x, y, 20.0, 20.0);
    ui::button(batch, atlas, x, y, 20.0, 20.0, &Text::literal(""), true, hover);
    if let Some(i) = icon {
        draw_icon(batch, atlas, i, x + 2.0, y + 2.0, 16.0);
    }
}

/// Cut `text` to fit `width`.
fn fit(atlas: &horizon_client::hud::HudAtlas, text: &str, width: f32) -> String {
    let mut out = text.to_string();
    while atlas.font.width_text(&Text::literal(out.clone())) > width && out.chars().count() > 1 {
        out.pop();
    }
    out
}

pub(super) fn screen_is_open() -> bool {
    SCREEN.with(|s| s.borrow().is_some())
}

pub(super) fn screen_open(game: &mut crate::game::Game<'_>) {
    set_language(game);
    SCREEN.with(|s| {
        *s.borrow_mut() = Some(ScreenState {
            tab: Tab::Devices,
            scroll: 0.0,
            dragging: None,
            choice: None,
            binding: None,
            search: ui::EditBox::new(0.0, 0.0, 360.0, 20.0),
            searching: false,
        })
    });
    with(|e| e.release_keys());
    game.capture(false);
}

pub(super) fn screen_close(game: &mut crate::game::Game<'_>) {
    SCREEN.with(|s| s.borrow_mut().take());
    if game.player.is_some() && game.screen == crate::game::Screen::Playing && game.title.is_none() {
        game.capture(true);
    }
}

/// The world went away: the screen with it, as dialogs go.
pub(super) fn screen_forget() {
    SCREEN.with(|s| s.borrow_mut().take());
}

fn with_state<R>(f: impl FnOnce(&mut ScreenState) -> R) -> Option<R> {
    SCREEN.with(|s| s.borrow_mut().as_mut().map(f))
}

pub(super) fn screen_draw(game: &mut crate::game::Game<'_>, batch: &mut HudBatch) {
    let Some((tab, scroll, choice, binding, search, searching)) = with_state(|s| {
        (s.tab, s.scroll, s.choice.clone(), s.binding.clone(), s.search.clone(), s.searching)
    }) else {
        return;
    };
    let (w, h) = (batch.width, batch.height);
    // Under an open list nothing else lights up.
    let cursor = if choice.is_some() { (-1.0, -1.0) } else { game.cursor };
    let s = settings(game);
    let rows = items(game, tab, &search.value);
    let in_world = game.player.is_some();
    let atlas = &game.gui.atlas;
    ui::menu_background(batch, atlas, 0.0, 0.0, w, h, in_world);
    batch.fill(atlas, 0.0, 0.0, w, TOP - 4.0, rgba(0, 0, 0, 110));
    // The title, the tabs and the two switches.
    let title = Text::literal(format!("Proximity Voice {VERSION}"));
    batch.text_styled(atlas, &title, 8.0, 12.0, u32::MAX, true);
    let mut x = 16.0 + atlas.font.width_text(&title);
    for (t, label, icon) in TABS {
        let text = Text::literal(tr(label));
        let tw = atlas.font.width_text(&text) + 22.0;
        let hover = ui::hovered(cursor, x, 6.0, tw, 20.0);
        ui::button(batch, atlas, x, 6.0, tw, 20.0, &Text::literal(""), t != tab, hover);
        let disabled = if t == tab { icons::icon(&format!("{icon}_disabled")) } else { None };
        if let Some(i) = disabled.or_else(|| icons::icon(icon)) {
            draw_icon(batch, atlas, i, x + 5.0, 12.0, 8.0);
        }
        let color = if t == tab { rgba(160, 160, 160, 255) } else { u32::MAX };
        batch.text_styled(atlas, &text, x + 16.0, 12.0, color, true);
        x += tw + 4.0;
    }
    let mic = if s.microphone_muted { "microphone_muted" } else { "microphone" };
    let speaker = if s.deafened { "speaker_muted" } else { "speaker" };
    icon_button(batch, atlas, cursor, icons::icon(mic), w - 50.0, 6.0);
    icon_button(batch, atlas, cursor, icons::icon(speaker), w - 26.0, 6.0);
    // The rows, scrolled and clipped to the body.
    let c = columns(w);
    let (rows, _) = placed(rows, scroll);
    let clip = batch.vertices.len();
    for (item, y) in rows {
        if y + ROW <= TOP || y >= h {
            continue;
        }
        match item {
            Item::Header(text) => {
                ui::centered_text(batch, atlas, &Text::literal(tr(&text)), 0.0, y + 6.0, w, u32::MAX);
            }
            Item::Search => {
                let mut edit = search.clone();
                edit.focused = searching;
                [edit.x, edit.y, edit.width, edit.height] = [c.label, y, c.reset + 20.0 - c.label, 20.0];
                edit.draw(batch, atlas, None);
            }
            Item::Row { id, icon, label, control, side } => {
                let mut lx = c.label;
                if let Some(i) = icon {
                    draw_icon(batch, atlas, i, lx, y + 2.0, 16.0);
                    lx += 20.0;
                }
                let label = fit(atlas, &label, c.control - lx - 6.0);
                batch.text_styled(atlas, &Text::literal(label), lx, y + 6.0, u32::MAX, true);
                let width = control_width(&c, &side);
                let hover = ui::hovered(cursor, c.control, y, width, 20.0);
                match &control {
                    Control::Slider(v, text) => {
                        ui::slider(batch, atlas, c.control, y, width, &Text::literal(text.clone()), *v, hover, true);
                    }
                    Control::Switch(b) => {
                        let v = if *b { 1.0 } else { 0.0 };
                        ui::slider(batch, atlas, c.control, y, width, &Text::literal(tr(on_off(*b))), v, hover, true);
                    }
                    Control::Choice(options, i) => {
                        ui::button(batch, atlas, c.control, y, width, 20.0, &Text::literal(""), true, hover);
                        let shown = fit(atlas, &tr(options.get(*i).map_or("", String::as_str)), width - 22.0);
                        batch.text_styled(atlas, &Text::literal(shown), c.control + 6.0, y + 6.0, u32::MAX, true);
                        // The list's arrow.
                        for r in 0..4 {
                            let r = r as f32;
                            batch.fill(atlas, c.control + width - 13.0 + r, y + 8.0 + r, 7.0 - 2.0 * r, 1.0, u32::MAX);
                        }
                    }
                    Control::Key(key) => {
                        let text = if binding.as_deref().is_some() && binding.as_deref() == key_name(&id) {
                            Text::colored(format!("> {} <", label_short(key)), 0xFFFF55)
                        } else {
                            Text::literal(label_short(key))
                        };
                        ui::button(batch, atlas, c.control, y, width, 20.0, &text, true, hover);
                    }
                }
                if let Some(side) = side {
                    let icon = match side {
                        Side::Mute(true) => "speaker_muted",
                        Side::Mute(false) => "speaker",
                        Side::Test(true) => "microphone_menu",
                        Side::Test(false) => "microphone_menu_disabled",
                    };
                    icon_button(batch, atlas, cursor, icons::icon(icon), c.side, y);
                }
                icon_button(batch, atlas, cursor, icons::icon("reset"), c.reset, y);
            }
        }
    }
    batch.clip_since(clip, 0.0, TOP, w, h);
    // An open list, over everything.
    if let Some((_, options, [x, y, lw, lh])) = choice {
        batch.fill(atlas, x - 1.0, y - 1.0, lw + 2.0, lh + 2.0, rgba(255, 255, 255, 255));
        batch.fill(atlas, x, y, lw, lh, rgba(0, 0, 0, 255));
        for (i, o) in options.iter().enumerate().take(12) {
            let oy = y + i as f32 * 14.0;
            if ui::hovered(game.cursor, x, oy, lw, 14.0) {
                batch.fill(atlas, x, oy, lw, 14.0, rgba(80, 80, 80, 255));
            }
            batch.text_styled(atlas, &Text::literal(fit(atlas, &tr(o), lw - 8.0)), x + 4.0, oy + 3.0, u32::MAX, true);
        }
    }
}

/// A key's name for a key button: `key.keyboard.v` is V; unbound is "None".
fn label_short(key: &str) -> String {
    if key == "key.keyboard.unknown" { tr("None") } else { label(key) }
}

fn key_name(id: &Id) -> Option<&str> {
    match id {
        Id::Key(name) => Some(name),
        _ => None,
    }
}

fn set_distance(s: &mut Settings, name: &str, d: i32) {
    let mut mine = s.activation(name);
    mine.distance = d;
    s.activations.insert(name.to_string(), mine);
    with(|e| e.show_distance(d));
}

/// A slider's value at 0..1 into its setting.
fn set_slider(game: &crate::game::Game<'_>, id: &Id, v: f32) {
    let mut s = settings(game);
    let o = overlay(game);
    match id {
        Id::Threshold => s.threshold_db = (v * 60.0 - 60.0).round(),
        Id::MicrophoneGain => s.microphone_gain = (v * 40.0).round() / 20.0,
        Id::Volume => s.volume = (v * 40.0).round() / 20.0,
        Id::NoiseGateDb => s.noise_gate_db = (v * 60.0 - 80.0).round(),
        Id::Line(name) => {
            s.line_volumes.insert(name.clone(), (v * 40.0).round() / 20.0);
        }
        Id::Player(p) => {
            s.player_volumes.insert(*p, (v * 40.0).round() / 20.0);
        }
        Id::Distance(name) => {
            let Some(a) = with(|e| e.activations().into_iter().find(|a| &a.name == name)).flatten() else { return };
            if let [-1, max] = a.distances.as_slice() {
                let d = 1 + (v * (*max - 1) as f32).round() as i32;
                set_distance(&mut s, name, d);
            }
        }
        _ => return,
    }
    save(game, s, o);
}

/// A switch flipped, or a list's option chosen.
fn set_choice(game: &crate::game::Game<'_>, id: &Id, index: Option<usize>) {
    let mut s = settings(game);
    let mut o = overlay(game);
    match id {
        Id::InputDisabled => s.input_disabled = !s.input_disabled,
        Id::NoiseGate => s.noise_gate = !s.noise_gate,
        Id::Compressor => s.compressor = !s.compressor,
        Id::Limiter => s.limiter = !s.limiter,
        Id::Exponential => s.exponential_distance = !s.exponential_distance,
        Id::Occlusion => s.occlusion = !s.occlusion,
        Id::Panning => s.panning = !s.panning,
        Id::Hud => o.shown = !o.shown,
        Id::Speakers => o.speakers = !o.speakers,
        Id::Nametags => o.nametags = !o.nametags,
        Id::Toggle(name) => {
            let mut mine = s.activation(name);
            mine.toggle = !mine.toggle;
            s.activations.insert(name.clone(), mine);
        }
        Id::Input | Id::Output => {
            let Some(i) = index else { return };
            let input = *id == Id::Input;
            let names = if input { Engine::input_devices() } else { Engine::output_devices() };
            let d = if i == 0 { None } else { names.get(i - 1).cloned() };
            if input {
                s.input_device = d;
            } else {
                s.output_device = d;
            }
        }
        Id::Corner => o.corner = index.unwrap_or(o.corner).min(3),
        Id::Mode(name) => {
            let Some(i) = index else { return };
            let proximity = name == voice::proto::PROXIMITY;
            let mode = match (proximity, i) {
                (true, 0) | (false, 1) => Mode::PushToTalk,
                (true, _) | (false, 2) => Mode::Voice,
                _ => Mode::Inherit,
            };
            let mut mine = s.activation(name);
            mine.mode = mode;
            s.activations.insert(name.clone(), mine);
        }
        Id::Distance(name) => {
            let Some(i) = index else { return };
            let Some(a) = with(|e| e.activations().into_iter().find(|a| &a.name == name)).flatten() else { return };
            if let Some(d) = a.distances.get(i) {
                set_distance(&mut s, name, *d);
            }
        }
        _ => return,
    }
    save(game, s, o);
}

/// The reset button: the setting back to its default.
fn reset(game: &mut crate::game::Game<'_>, id: &Id) {
    let d = Settings::default();
    let dov = Overlay::default();
    let mut s = settings(game);
    let mut o = overlay(game);
    match id {
        Id::Threshold => s.threshold_db = d.threshold_db,
        Id::Input => s.input_device = None,
        Id::MicrophoneGain => s.microphone_gain = d.microphone_gain,
        Id::InputDisabled => s.input_disabled = d.input_disabled,
        Id::Output => s.output_device = None,
        Id::Volume => s.volume = d.volume,
        Id::NoiseGate => s.noise_gate = d.noise_gate,
        Id::NoiseGateDb => s.noise_gate_db = d.noise_gate_db,
        Id::Compressor => s.compressor = d.compressor,
        Id::Limiter => s.limiter = d.limiter,
        Id::Exponential => s.exponential_distance = d.exponential_distance,
        Id::Occlusion => s.occlusion = d.occlusion,
        Id::Panning => s.panning = d.panning,
        Id::Line(name) => {
            s.line_volumes.remove(name);
        }
        Id::Player(p) => {
            s.player_volumes.remove(p);
            s.muted_players.remove(p);
        }
        Id::Mode(name) | Id::Toggle(name) | Id::Distance(name) => {
            s.activations.remove(name);
        }
        Id::Hud => o.shown = dov.shown,
        Id::Corner => o.corner = dov.corner,
        Id::Speakers => o.speakers = dov.speakers,
        Id::Nametags => o.nametags = dov.nametags,
        Id::Key(name) => {
            if let Some(keys) = game.settings.mods.keys.get_mut(ID) {
                keys.remove(name);
            }
            game.settings.save(&crate::game::Game::options_path());
            return;
        }
    }
    save(game, s, o);
}

/// The side button: a line's or player's mute, or the microphone test.
fn side_click(game: &crate::game::Game<'_>, id: &Id) {
    let mut s = settings(game);
    let o = overlay(game);
    match id {
        Id::Threshold => s.monitor = !s.monitor,
        Id::Line(name) => {
            if s.line_volumes.get(name) == Some(&0.0) {
                s.line_volumes.remove(name);
            } else {
                s.line_volumes.insert(name.clone(), 0.0);
            }
        }
        Id::Player(p) => {
            if !s.muted_players.remove(p) {
                s.muted_players.insert(*p);
            }
        }
        _ => return,
    }
    save(game, s, o);
}

pub(super) fn screen_click(game: &mut crate::game::Game<'_>) {
    let cursor = game.cursor;
    let (w, h) = game.gui_size();
    let Some((tab, scroll, choice, search)) =
        with_state(|s| (s.tab, s.scroll, s.choice.clone(), s.search.value.clone()))
    else {
        return;
    };
    // An open list takes the click: an option, or anywhere else to close it.
    if let Some((id, options, [x, y, lw, _])) = choice {
        let i = ((cursor.1 - y) / 14.0).floor();
        if cursor.0 >= x && cursor.0 < x + lw && i >= 0.0 && (i as usize) < options.len().min(12) {
            game.audio.play_button_click();
            set_choice(game, &id, Some(i as usize));
        }
        with_state(|s| s.choice = None);
        return;
    }
    with_state(|s| {
        s.binding = None;
        s.searching = false;
    });
    // The header: tabs, then the two switches.
    if cursor.1 < TOP {
        let mut x = 16.0 + game.gui.atlas.font.width_text(&Text::literal(format!("Proximity Voice {VERSION}")));
        for (t, label, _) in TABS {
            let tw = game.gui.atlas.font.width_text(&Text::literal(tr(label))) + 22.0;
            if ui::hovered(cursor, x, 6.0, tw, 20.0) {
                game.audio.play_button_click();
                with_state(|s| {
                    s.tab = t;
                    s.scroll = 0.0;
                });
                return;
            }
            x += tw + 4.0;
        }
        let mut s = settings(game);
        if ui::hovered(cursor, w - 50.0, 6.0, 20.0, 20.0) {
            game.audio.play_button_click();
            s.microphone_muted = !s.microphone_muted;
            save_settings(game, s);
        } else if ui::hovered(cursor, w - 26.0, 6.0, 20.0, 20.0) {
            game.audio.play_button_click();
            s.deafened = !s.deafened;
            save_settings(game, s);
        }
        return;
    }
    let c = columns(w);
    let (rows, _) = placed(items(game, tab, &search), scroll);
    for (item, y) in rows {
        if y + ROW <= TOP || y >= h || cursor.1 < y || cursor.1 >= y + 20.0 {
            continue;
        }
        match item {
            Item::Search => {
                if cursor.0 >= c.label && cursor.0 < c.reset + 20.0 {
                    with_state(|s| s.searching = true);
                }
            }
            Item::Row { id, control, side, .. } => {
                let width = control_width(&c, &side);
                if ui::hovered(cursor, c.reset, y, 20.0, 20.0) {
                    game.audio.play_button_click();
                    reset(game, &id);
                } else if side.is_some() && ui::hovered(cursor, c.side, y, 20.0, 20.0) {
                    game.audio.play_button_click();
                    side_click(game, &id);
                } else if ui::hovered(cursor, c.control, y, width, 20.0) {
                    game.audio.play_button_click();
                    match control {
                        Control::Slider(..) => {
                            set_slider(game, &id, ui::slider_value(c.control, width, cursor.0));
                            with_state(|s| s.dragging = Some(id.clone()));
                        }
                        Control::Switch(_) => set_choice(game, &id, None),
                        Control::Choice(options, _) => {
                            let lh = options.len().min(12) as f32 * 14.0;
                            let below = y + 21.0;
                            let ly = if below + lh > h { (y - lh - 1.0).max(TOP) } else { below };
                            let rect = [c.control, ly, width.max(180.0), lh];
                            with_state(|s| s.choice = Some((id.clone(), options, rect)));
                        }
                        Control::Key(_) => {
                            let name = key_name(&id).map(str::to_string);
                            with_state(|s| s.binding = name);
                        }
                    }
                }
            }
            Item::Header(_) => {}
        }
        return;
    }
}

pub(super) fn screen_drag(game: &mut crate::game::Game<'_>) {
    let Some((Some(id), tab, search)) = with_state(|s| (s.dragging.clone(), s.tab, s.search.value.clone())) else {
        return;
    };
    let (w, _) = game.gui_size();
    let c = columns(w);
    let side = items(game, tab, &search)
        .into_iter()
        .find_map(|i| match i {
            Item::Row { id: r, side, .. } if r == id => Some(side),
            _ => None,
        })
        .flatten();
    let width = control_width(&c, &side);
    set_slider(game, &id, ui::slider_value(c.control, width, game.cursor.0));
}

pub(super) fn screen_release() {
    with_state(|s| s.dragging = None);
}

pub(super) fn screen_scroll(game: &mut crate::game::Game<'_>, delta: f32) {
    let (_, h) = game.gui_size();
    let Some((tab, search)) = with_state(|s| (s.tab, s.search.value.clone())) else {
        return;
    };
    let (_, content) = placed(items(game, tab, &search), 0.0);
    let max = (content - (h - TOP - 8.0)).max(0.0);
    with_state(|s| {
        if s.choice.is_none() {
            s.scroll = (s.scroll - delta * 20.0).clamp(0.0, max);
        }
    });
}

/// A key on the screen: a key button takes it (Escape unbinds), the player search types it,
/// Escape closes the list or the screen.
pub(super) fn screen_key(game: &mut crate::game::Game<'_>, key: winit::keyboard::KeyCode, text: Option<&str>) {
    use winit::keyboard::KeyCode;
    let Some((binding, searching, choice)) = with_state(|s| (s.binding.clone(), s.searching, s.choice.is_some())) else {
        return;
    };
    if let Some(name) = binding {
        let chosen = if key == KeyCode::Escape {
            "key.keyboard.unknown"
        } else {
            horizon_client::keybinds::key_name(key).unwrap_or("key.keyboard.unknown")
        };
        game.settings.mods.keys.entry(ID.to_string()).or_default().insert(name, chosen.to_string());
        game.settings.save(&crate::game::Game::options_path());
        with_state(|s| s.binding = None);
        return;
    }
    if key == KeyCode::Escape {
        if choice {
            with_state(|s| s.choice = None);
        } else {
            screen_close(game);
        }
        return;
    }
    if searching {
        with_state(|s| {
            if !s.search.key(key, false, false)
                && let Some(t) = text.filter(|t| t.chars().all(|c| !c.is_control()))
            {
                s.search.insert_text(t);
            }
        });
    }
}
