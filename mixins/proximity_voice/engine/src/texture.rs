//! Icons as a server names them: one of Plasmo Voice's own by its texture path, or an add-on's
//! own picture sent inline (`base64;<PNG>`, `VoiceIconUtil.getIcon`), decoded once and kept.
use crate::icons::{self, Icon};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The largest side kept of a sent picture: icons draw at 16 pixels.
const MAX_SIDE: usize = 32;

static SENT: OnceLock<Mutex<HashMap<String, Option<&'static Icon>>>> = OnceLock::new();

/// The icon `name` stands for, if this client has or can read it.
pub fn icon(name: &str) -> Option<&'static Icon> {
    let Some(data) = name.strip_prefix("base64;") else {
        return icons::icon(name);
    };
    let cache = SENT.get_or_init(Default::default);
    let mut cache = cache.lock().ok()?;
    *cache.entry(name.to_string()).or_insert_with(|| {
        // Each distinct picture is decoded once for the whole game: a handful per server.
        decode(data).map(|(size, pixels)| -> &'static Icon {
            Box::leak(Box::new(Icon { name: "sent", size, pixels: Box::leak(pixels.into_boxed_slice()) }))
        })
    })
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut n) = (0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        bits = bits << 6 | value(c)?;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    Some(out)
}

/// A PNG's pixels as ARGB, square (its top-left square) and at most `MAX_SIDE` a side.
fn decode(data: &str) -> Option<(usize, Vec<u32>)> {
    let bytes = base64(data)?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let channels = frame.color_type.samples();
    let pixel = |x: usize, y: usize| -> u32 {
        let p = &buf[(y * w + x) * channels..][..channels];
        let (r, g, b, a) = match channels {
            1 => (p[0], p[0], p[0], 255),
            2 => (p[0], p[0], p[0], p[1]),
            3 => (p[0], p[1], p[2], 255),
            _ => (p[0], p[1], p[2], p[3]),
        };
        u32::from(a) << 24 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
    };
    let side = w.min(h);
    if side == 0 {
        return None;
    }
    let size = side.min(MAX_SIDE);
    let mut pixels = Vec::with_capacity(size * size);
    for y in 0..size {
        for x in 0..size {
            pixels.push(pixel(x * side / size, y * side / size));
        }
    }
    Some((size, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVs bG8").unwrap(), b"hello");
        assert!(base64("a*b").is_none());
    }

    #[test]
    fn named_icons_and_sent_ones() {
        assert_eq!(icon("plasmovoice:textures/icons/speaker_disc.png").unwrap().name, "speaker_disc");
        // A 1x1 red PNG.
        let red = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
        let i = icon(&format!("base64;{red}")).unwrap();
        assert_eq!((i.size, i.pixels[0] >> 24), (1, 0xFF));
        assert!(icon("base64;not a png").is_none());
    }
}
