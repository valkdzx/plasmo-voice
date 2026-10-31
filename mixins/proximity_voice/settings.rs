// The mod's settings page: one button per setting.
fn proximity_voice_page(game: &mut crate::game::Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    use crate::game::proximity_voice as m;
    if mod_id == m::ID {
        buttons.extend(m::page(game));
    }
}
