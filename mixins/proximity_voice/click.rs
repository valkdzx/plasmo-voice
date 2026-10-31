// A click on the settings page: the setting changes and is kept.
fn proximity_voice_click(game: &mut crate::game::Game<'_>, mod_id: &str, button: usize) {
    use crate::game::proximity_voice as m;
    if mod_id == m::ID {
        m::click(game, button);
    }
}
