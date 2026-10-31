// Typed text reaches the mod's screen with its key press (screen_key.rs): nothing more here.
fn proximity_voice_screen_type() -> std::ops::ControlFlow<bool> {
    if crate::game::proximity_voice::screen_is_open() {
        std::ops::ControlFlow::Break(true)
    } else {
        std::ops::ControlFlow::Continue(())
    }
}
