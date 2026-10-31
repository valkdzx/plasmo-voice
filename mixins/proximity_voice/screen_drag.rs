// The cursor moved over the mod's screen: a held slider follows it.
fn proximity_voice_screen_drag(game: &mut crate::game::Game<'_>) -> std::ops::ControlFlow<()> {
    if !crate::game::proximity_voice::screen_is_open() {
        return std::ops::ControlFlow::Continue(());
    }
    crate::game::proximity_voice::screen_drag(game);
    std::ops::ControlFlow::Break(())
}
