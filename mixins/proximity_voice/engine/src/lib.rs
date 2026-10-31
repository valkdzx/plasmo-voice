//! Proximity Voice's engine: a Plasmo Voice 2.x client without the game. It reads and writes
//! the voice server's packets, keeps the UDP link, encodes the microphone with Opus and plays
//! every source through the speakers where the game says it is. The mod's code in the game
//! (`crate::game::proximity_voice`) feeds it the channel's payloads and the world.
//!
//! A port of Plasmo Voice's client (https://github.com/plasmoapp/plasmo-voice, LGPL-3.0) by
//! the Proximity Voice authors, under the same licence; `opus` is rusty-opus (BSD-3-Clause).

pub mod aes;
mod audio;
pub mod codec;
pub mod dsp;
pub mod icons;
mod engine;
pub mod jio;
pub mod md5;
pub mod mixer;
// Vendored as published: its own lints are not this crate's.
#[allow(warnings, clippy::all, missing_docs)]
pub mod opus;
pub mod proto;
pub mod texture;

pub use engine::{
    ActivationSettings, ActivationView, Connection, Engine, LineView, Listener, Locator, Mode, PROTOCOL_VERSION, RsaKey,
    Settings, Status, spatial,
};
pub use proto::VoicePlayer;
