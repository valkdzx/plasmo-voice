// Proximity Voice's marks for the names over heads: who speaks and who is silent, set by the
// mod's game side (crate::game::proximity_voice in horizon-client) each tick, read by the hook
// on HudBatch::name_tag on the same thread.
use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    static MARKS: RefCell<(bool, HashSet<String>, HashSet<String>)> = RefCell::new(Default::default());
}

/// The players speaking and the silent ones (muted, or with voice or microphone off), and
/// whether marks show at all.
pub fn set(shown: bool, speaking: HashSet<String>, silent: HashSet<String>) {
    MARKS.with(|m| *m.borrow_mut() = (shown, speaking, silent));
}

const SPEAKING: &str = "\u{266A} ";
const SILENT: &str = "\u{2716} ";

/// The name with its mark in front: a green note while the player speaks, a red cross while
/// they are silent. None for an unmarked name, or one marked already.
pub fn marked(text: &crate::text::Text) -> Option<crate::text::Text> {
    let plain = text.plain();
    if plain.starts_with(SPEAKING) || plain.starts_with(SILENT) {
        return None;
    }
    MARKS.with(|m| {
        let (shown, speaking, silent) = &*m.borrow();
        let (mark, rgb) = if !*shown {
            return None;
        } else if speaking.iter().any(|n| plain.contains(n.as_str())) {
            (SPEAKING, 0x55FF55)
        } else if silent.iter().any(|n| plain.contains(n.as_str())) {
            (SILENT, 0xFF5555)
        } else {
            return None;
        };
        Some(crate::text::Text::colored(mark, rgb).append(text.clone()))
    })
}
