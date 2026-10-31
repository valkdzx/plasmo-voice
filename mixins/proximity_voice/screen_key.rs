// A key pressed on the mod's screen, with the text it types.
fn proximity_voice_screen_key(
    game: &mut crate::game::Game<'_>,
    key: winit::keyboard::KeyCode,
    text: Option<&str>,
) -> std::ops::ControlFlow<Result<()>> {
    if !crate::game::proximity_voice::screen_is_open() {
        return std::ops::ControlFlow::Continue(());
    }
    crate::game::proximity_voice::screen_key(game, key, text);
    std::ops::ControlFlow::Break(Ok(()))
}
