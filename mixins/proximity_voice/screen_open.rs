// While the mod's screen is open the game takes it for an open dialog: the frame and the
// input go where a dialog's would.
fn proximity_voice_screen_open() -> std::ops::ControlFlow<bool> {
    if crate::game::proximity_voice::screen_is_open() {
        std::ops::ControlFlow::Break(true)
    } else {
        std::ops::ControlFlow::Continue(())
    }
}
