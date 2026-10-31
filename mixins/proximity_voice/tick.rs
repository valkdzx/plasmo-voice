// The engine follows the server, the player and the sources, and its packets go out.
fn proximity_voice_tick(game: &mut crate::game::Game<'_>) {
    crate::game::proximity_voice::tick(game);
}
