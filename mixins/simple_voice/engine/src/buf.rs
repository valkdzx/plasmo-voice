//! Minecraft's `FriendlyByteBuf`, as Simple Voice Chat writes its packets with it: big-endian
//! numbers, VarInt lengths, UUIDs as two longs and strings as VarInt-prefixed UTF-8.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            return Err(Malformed("packet too short"));
        }
        let out = &self.data[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().unwrap())
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.array()?))
    }
    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }
    pub fn uuid(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }
    pub fn varint(&mut self) -> Result<i32> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            value |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(Malformed("VarInt too long"))
    }
    /// `readByteArray(max)`.
    pub fn byte_array(&mut self, max: usize) -> Result<&'a [u8]> {
        let n = self.varint()?;
        if n < 0 || n as usize > max {
            return Err(Malformed("byte array too long"));
        }
        self.bytes(n as usize)
    }
    /// `readUtf(max)`: at most `max` characters.
    pub fn utf(&mut self, max: usize) -> Result<String> {
        let n = self.varint()?;
        if n < 0 || n as usize > max * 3 {
            return Err(Malformed("string too long"));
        }
        let s = std::str::from_utf8(self.bytes(n as usize)?).map_err(|_| Malformed("bad UTF-8"))?;
        if s.encode_utf16().count() > max {
            return Err(Malformed("string too long"));
        }
        Ok(s.to_string())
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
        self.raw(&v.to_be_bytes())
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.raw(&v.to_be_bytes())
    }
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.raw(&v.to_be_bytes())
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.raw(&v.to_be_bytes())
    }
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.raw(&v.to_be_bytes())
    }
    pub fn uuid(&mut self, v: u128) -> &mut Self {
        self.raw(&v.to_be_bytes())
    }
    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.data.extend_from_slice(v);
        self
    }
    pub fn varint(&mut self, v: i32) -> &mut Self {
        let mut v = v as u32;
        loop {
            if v & !0x7F == 0 {
                self.data.push(v as u8);
                return self;
            }
            self.data.push((v & 0x7F) as u8 | 0x80);
            v >>= 7;
        }
    }
    pub fn byte_array(&mut self, v: &[u8]) -> &mut Self {
        self.varint(v.len() as i32).raw(v)
    }
    pub fn utf(&mut self, s: &str) -> &mut Self {
        self.byte_array(s.as_bytes())
    }
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for v in [0, 1, 127, 128, 255, 25565, 2097151, i32::MAX, -1, i32::MIN] {
            let mut w = Writer::new();
            w.varint(v);
            assert_eq!(Reader::new(&w.data).varint(), Ok(v));
        }
        let mut w = Writer::new();
        w.varint(300);
        assert_eq!(w.data, [0xAC, 0x02]);
    }

    #[test]
    fn strings_and_uuids() {
        let mut w = Writer::new();
        w.utf("Привет").uuid(0x0123_4567_89ab_cdef_0011_2233_4455_6677).bool(true);
        let mut r = Reader::new(&w.data);
        assert_eq!(r.utf(16).unwrap(), "Привет");
        assert_eq!(r.uuid().unwrap(), 0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        assert!(r.bool().unwrap());
        assert_eq!(r.remaining(), 0);
        assert!(Reader::new(&w.data).utf(3).is_err());
    }
}
