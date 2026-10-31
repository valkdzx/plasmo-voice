// A menu button's `/vc apply …` (a dialog runs its command as chat): it stays here instead of
// going to the server.
fn proximity_voice_chat(game: &mut crate::game::Game<'_>, message: &str) -> std::ops::ControlFlow<()> {
    if crate::game::proximity_voice::chat(game, message) {
        std::ops::ControlFlow::Break(())
    } else {
        std::ops::ControlFlow::Continue(())
    }
}
