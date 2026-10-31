// The talk key (held) and the mute key (pressed).
fn proximity_voice_key(game: &mut crate::game::Game<'_>, key: &str, pressed: bool) {
    crate::game::proximity_voice::key(game, key, pressed);
}
