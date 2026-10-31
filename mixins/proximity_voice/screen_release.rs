// The left button let go over the mod's screen.
fn proximity_voice_screen_release() -> std::ops::ControlFlow<()> {
    if !crate::game::proximity_voice::screen_is_open() {
        return std::ops::ControlFlow::Continue(());
    }
    crate::game::proximity_voice::screen_release();
    std::ops::ControlFlow::Break(())
}
