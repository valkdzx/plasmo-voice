// The mod's row in Options > Mods, with its keys: the fixed ones and one per activation the
// server offers (groups and other add-ons).
fn proximity_voice_listed(mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    use crate::game::proximity_voice as m;
    mods.push(crate::game::native_hooks::NativeMod {
        id: m::ID.into(),
        title: format!("Proximity Voice {}", m::VERSION),
        description: "Voice chat on servers with Plasmo Voice: hear players near you, talk to them.".into(),
        keys: m::keys(),
    });
}
