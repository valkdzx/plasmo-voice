//! AES-128/GCM/NoPadding with a 96-bit IV and a 128-bit tag, as Simple Voice Chat's `Secret`
//! encrypts every voice packet: the payload is the IV, then the ciphertext and its tag.
use crate::aes::Aes128;

pub const IV_SIZE: usize = 12;
pub const TAG_SIZE: usize = 16;

pub struct Gcm {
    aes: Aes128,
    h: u128,
}

/// Multiplication in GF(2^128) as GCM defines it (bits reflected).
fn gmul(x: u128, y: u128) -> u128 {
    const R: u128 = 0xE1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for i in 0..128 {
        if (x >> (127 - i)) & 1 == 1 {
            z ^= v;
        }
        v = if v & 1 == 1 { (v >> 1) ^ R } else { v >> 1 };
    }
    z
}

impl Gcm {
    pub fn new(key: &[u8; 16]) -> Self {
        let aes = Aes128::new(key);
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);
        Self { aes, h: u128::from_be_bytes(h) }
    }

    fn ghash(&self, data: &[u8]) -> u128 {
        let mut x = 0u128;
        for chunk in data.chunks(16) {
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            x = gmul(x ^ u128::from_be_bytes(block), self.h);
        }
        // No additional data: the lengths block is 0 and the ciphertext's bits.
        gmul(x ^ (data.len() as u128 * 8), self.h)
    }

    fn block(&self, counter: u128) -> [u8; 16] {
        let mut b = counter.to_be_bytes();
        self.aes.encrypt_block(&mut b);
        b
    }

    /// Counter mode from the block after J0.
    fn ctr(&self, j0: u128, data: &mut [u8]) {
        for (i, chunk) in data.chunks_mut(16).enumerate() {
            let low = (j0 as u32).wrapping_add(i as u32 + 1);
            let counter = (j0 & !0xFFFF_FFFFu128) | low as u128;
            let k = self.block(counter);
            for (b, k) in chunk.iter_mut().zip(k) {
                *b ^= k;
            }
        }
    }

    fn j0(iv: &[u8; IV_SIZE]) -> u128 {
        let mut b = [0u8; 16];
        b[..IV_SIZE].copy_from_slice(iv);
        b[15] = 1;
        u128::from_be_bytes(b)
    }

    /// The ciphertext with its tag after it, for `iv`.
    pub fn seal(&self, iv: &[u8; IV_SIZE], plain: &[u8]) -> Vec<u8> {
        let j0 = Self::j0(iv);
        let mut out = plain.to_vec();
        self.ctr(j0, &mut out);
        let tag = self.ghash(&out) ^ u128::from_be_bytes(self.block(j0));
        out.extend_from_slice(&tag.to_be_bytes());
        out
    }

    /// The plain text of a ciphertext with its tag, if the tag is right.
    pub fn open(&self, iv: &[u8; IV_SIZE], sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < TAG_SIZE {
            return None;
        }
        let (data, tag) = sealed.split_at(sealed.len() - TAG_SIZE);
        let j0 = Self::j0(iv);
        let expected = (self.ghash(data) ^ u128::from_be_bytes(self.block(j0))).to_be_bytes();
        // Compared in full, not stopping at the first difference.
        if expected.iter().zip(tag).fold(0u8, |d, (a, b)| d | (a ^ b)) != 0 {
            return None;
        }
        let mut out = data.to_vec();
        self.ctr(j0, &mut out);
        Some(out)
    }

    /// `Secret.encrypt`: a fresh random IV, then the sealed data.
    pub fn encrypt(&self, plain: &[u8]) -> Option<Vec<u8>> {
        let mut iv = [0u8; IV_SIZE];
        getrandom::getrandom(&mut iv).ok()?;
        let mut out = iv.to_vec();
        out.extend(self.seal(&iv, plain));
        Some(out)
    }

    /// `Secret.decrypt`.
    pub fn decrypt(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < IV_SIZE + TAG_SIZE {
            return None;
        }
        let iv: [u8; IV_SIZE] = payload[..IV_SIZE].try_into().ok()?;
        self.open(&iv, &payload[IV_SIZE..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // The GCM specification's test cases 1 to 3 (McGrew and Viega).
    #[test]
    fn empty_message() {
        let g = Gcm::new(&[0; 16]);
        assert_eq!(g.seal(&[0; 12], &[]), hex("58e2fccefa7e3061367f1d57a4e7455a"));
    }

    #[test]
    fn one_zero_block() {
        let g = Gcm::new(&[0; 16]);
        assert_eq!(
            g.seal(&[0; 12], &[0; 16]),
            hex("0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf")
        );
    }

    #[test]
    fn four_blocks() {
        let key: [u8; 16] = hex("feffe9928665731c6d6a8f9467308308").try_into().unwrap();
        let iv: [u8; 12] = hex("cafebabefacedbaddecaf888").try_into().unwrap();
        let plain = hex(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255",
        );
        let sealed = Gcm::new(&key).seal(&iv, &plain);
        assert_eq!(
            sealed,
            hex(
                "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091473f59854d5c2af327cd64a62cf35abd2ba6fab4"
            )
        );
        assert_eq!(Gcm::new(&key).open(&iv, &sealed), Some(plain));
    }

    #[test]
    fn tampering_is_refused() {
        let g = Gcm::new(&[7; 16]);
        let mut p = g.encrypt(b"hello voice").unwrap();
        assert_eq!(g.decrypt(&p).as_deref(), Some(&b"hello voice"[..]));
        let last = p.len() - 1;
        p[last] ^= 1;
        assert_eq!(g.decrypt(&p), None);
    }
}
