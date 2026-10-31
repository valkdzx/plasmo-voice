// /vc and its settings; every other command is left alone.
fn proximity_voice_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    crate::game::proximity_voice::command(game, command, ran);
}
