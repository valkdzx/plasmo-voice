//! Java's DataInput and DataOutput, as Plasmo Voice's packets use them (Guava's
//! ByteArrayDataInput/Output): big-endian numbers, booleans as one byte, UUIDs as their two
//! longs, and strings in modified UTF-8 behind a u16 length (`writeUTF`).

/// A packet that ended early or holds a value out of its range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

pub type Result<T> = std::result::Result<T, Malformed>;

pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.at
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Malformed("packet ends early"));
        }
        let out = &self.data[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("N bytes"))
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }
    /// A UUID: its most significant long, then its least significant one.
    pub fn uuid(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }
    /// `PacketUtil.readSafeInt`: an int that must lie in `min..=max`.
    pub fn int_in(&mut self, min: i32, max: i32) -> Result<i32> {
        let n = self.i32()?;
        if n < min || n > max {
            return Err(Malformed("int out of range"));
        }
        Ok(n)
    }
    /// A length read by `readSafeInt(0, max)`.
    pub fn len(&mut self, max: i32) -> Result<usize> {
        Ok(self.int_in(0, max)? as usize)
    }
    /// `readUTF`: modified UTF-8 (U+0000 as two bytes, characters past the BMP as their
    /// surrogates of three bytes each) behind a u16 byte length.
    pub fn utf(&mut self) -> Result<String> {
        let n = self.u16()? as usize;
        let bytes = self.bytes(n)?;
        let mut units = Vec::with_capacity(n);
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            let (unit, len) = match b >> 4 {
                0..=7 => (b as u16, 1),
                12 | 13 => {
                    let b1 = *bytes.get(i + 1).ok_or(Malformed("bad modified UTF-8"))?;
                    (((b as u16 & 0x1F) << 6) | (b1 as u16 & 0x3F), 2)
                }
                14 => {
                    let b1 = *bytes.get(i + 1).ok_or(Malformed("bad modified UTF-8"))?;
                    let b2 = *bytes.get(i + 2).ok_or(Malformed("bad modified UTF-8"))?;
                    (((b as u16 & 0x0F) << 12) | ((b1 as u16 & 0x3F) << 6) | (b2 as u16 & 0x3F), 3)
                }
                _ => return Err(Malformed("bad modified UTF-8")),
            };
            units.push(unit);
            i += len;
        }
        Ok(String::from_utf16_lossy(&units))
    }
    /// `readUTF` with a limit on its length in UTF-16 units (`PacketUtil.readSafeUTF`).
    pub fn utf_max(&mut self, max: usize) -> Result<String> {
        let s = self.utf()?;
        if s.encode_utf16().count() > max {
            return Err(Malformed("string too long"));
        }
        Ok(s)
    }
    /// `PacketUtil.readNullableString`.
    pub fn nullable_utf(&mut self) -> Result<Option<String>> {
        if self.bool()? { Ok(Some(self.utf()?)) } else { Ok(None) }
    }
}

#[derive(Default)]
pub struct Writer {
    pub data: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.data.push(v);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }
    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn uuid(&mut self, v: u128) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.data.extend_from_slice(v);
        self
    }
    /// `writeUTF`. A string longer than 65535 bytes of modified UTF-8 is cut at the last
    /// whole character that fits (Java throws instead; nothing here sends one).
    pub fn utf(&mut self, s: &str) -> &mut Self {
        let mut encoded = Vec::with_capacity(s.len());
        for unit in s.encode_utf16() {
            let start = encoded.len();
            match unit {
                0x0001..=0x007F => encoded.push(unit as u8),
                0x0000 | 0x0080..=0x07FF => {
                    encoded.push(0xC0 | (unit >> 6) as u8);
                    encoded.push(0x80 | (unit & 0x3F) as u8);
                }
                _ => {
                    encoded.push(0xE0 | (unit >> 12) as u8);
                    encoded.push(0x80 | ((unit >> 6) & 0x3F) as u8);
                    encoded.push(0x80 | (unit & 0x3F) as u8);
                }
            }
            if encoded.len() > u16::MAX as usize {
                encoded.truncate(start);
                break;
            }
        }
        self.u16(encoded.len() as u16);
        self.bytes(&encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_utf8_round_trips() {
        for s in ["", "proximity", "Привет", "a\0b", "🎤 voice"] {
            let mut w = Writer::new();
            w.utf(s);
            assert_eq!(Reader::new(&w.data).utf().unwrap(), s);
        }
        // Java writes U+0000 as C0 80 and an emoji as two 3-byte surrogates.
        let mut w = Writer::new();
        w.utf("\0🎤");
        assert_eq!(w.data, [0, 8, 0xC0, 0x80, 0xED, 0xA0, 0xBC, 0xED, 0xBE, 0xA4]);
    }

    #[test]
    fn numbers_are_big_endian() {
        let mut w = Writer::new();
        w.i32(0x4e9004e9).i16(-2).uuid(1);
        assert_eq!(&w.data[..6], &[0x4e, 0x90, 0x04, 0xe9, 0xFF, 0xFE]);
        let mut r = Reader::new(&w.data);
        assert_eq!(r.i32().unwrap(), 0x4e9004e9);
        assert_eq!(r.i16().unwrap(), -2);
        assert_eq!(r.uuid().unwrap(), 1);
        assert!(r.u8().is_err());
    }
}
