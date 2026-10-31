// The mod's screen, drawn in the dialog's place.
fn proximity_voice_screen_draw(
    game: &mut crate::game::Game<'_>,
    batch: &mut horizon_client::hud::HudBatch,
) -> std::ops::ControlFlow<()> {
    if !crate::game::proximity_voice::screen_is_open() {
        return std::ops::ControlFlow::Continue(());
    }
    crate::game::proximity_voice::screen_draw(game, batch);
    std::ops::ControlFlow::Break(())
}
