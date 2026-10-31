// A name over a head, drawn again with its mark in front when it has one.
fn proximity_voice_name_tag(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    text: &crate::text::Text,
    y: f32,
    color: u32,
    background: u32,
) -> std::ops::ControlFlow<usize> {
    match crate::proximity_marks::marked(text) {
        Some(marked) => std::ops::ControlFlow::Break(batch.name_tag(atlas, &marked, y, color, background)),
        None => std::ops::ControlFlow::Continue(()),
    }
}
