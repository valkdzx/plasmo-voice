// Every packet of play, before the game handles it: Plasmo Voice's channel, and the login
// that registers it. It also copies where the server put the entities sources hang on that no
// model shows (the tracked entities are this module's own).
fn proximity_voice_packet(
    game: &mut crate::game::Game<'_>,
    packet: &horizon_protocol::packets::play::Clientbound,
) {
    if let Some(r) = game.remote.as_ref() {
        for id in crate::game::proximity_voice::tracked_wanted() {
            if let Some(t) = r.entities.get(&id) {
                crate::game::proximity_voice::track(id, t.position);
            }
        }
    }
    crate::game::proximity_voice::packet(game, packet);
}
