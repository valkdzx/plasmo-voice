// A left click on the mod's screen.
fn proximity_voice_screen_click(game: &mut crate::game::Game<'_>) -> std::ops::ControlFlow<Result<()>> {
    if !crate::game::proximity_voice::screen_is_open() {
        return std::ops::ControlFlow::Continue(());
    }
    crate::game::proximity_voice::screen_click(game);
    std::ops::ControlFlow::Break(Ok(()))
}
