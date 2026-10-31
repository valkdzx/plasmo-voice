// The wheel on the mod's screen.
fn proximity_voice_screen_scroll(game: &mut crate::game::Game<'_>, delta: f32) -> std::ops::ControlFlow<()> {
    if !crate::game::proximity_voice::screen_is_open() {
        return std::ops::ControlFlow::Continue(());
    }
    crate::game::proximity_voice::screen_scroll(game, delta);
    std::ops::ControlFlow::Break(())
}
