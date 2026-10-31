// The voice's state and who is speaking, in the bottom right corner.
fn proximity_voice_hud(
    game: &mut crate::game::Game<'_>,
    batch: &mut horizon_client::hud::HudBatch,
    corners: &mut horizon_client::extras_hud::Corners,
) {
    crate::game::proximity_voice::hud(game, batch, corners);
}
