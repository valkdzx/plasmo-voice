//! Simple Voice's engine: a Simple Voice Chat 2.6 client without the game. It reads and
//! writes the voice chat's plugin messages and UDP packets, encodes the microphone with Opus
//! and plays every voice through the speakers where the game says it is. The mod's code in
//! the game (`crate::game::simple_voice`) feeds it the payloads and the world.
//!
//! Made for Horizon by the Simple Voice authors, after Simple Voice Chat's protocol
//! (https://github.com/henkelmax/simple-voice-chat); the audio, mixing and Opus parts come from
//! Proximity Voice. `opus` is rusty-opus (BSD-3-Clause).

pub mod aes;
mod audio;
pub mod buf;
pub mod codec;
pub mod dsp;
mod engine;
pub mod gcm;
pub mod icons;
pub mod mixer;
// Vendored as published: its own lints are not this crate's.
#[allow(warnings, clippy::all, missing_docs)]
pub mod opus;
pub mod proto;

pub use engine::{CENTRE, Connection, Engine, Listener, Mode, Settings, Status, spatial};
pub use proto::{Group, GroupType, PlayerState};
