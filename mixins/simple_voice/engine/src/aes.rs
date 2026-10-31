//! AES-128 and Plasmo Voice's `AesEncryption`: AES/CBC/PKCS5Padding with a random IV for
//! each voice packet, sent in front of the ciphertext.

const SBOX: [u8; 256] = {
    // The S-box from the field inverse and the affine map, as FIPS-197 defines it.
    let mut sbox = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p *= 3 in GF(2^8)
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1B } else { 0 };
        // q /= 3
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    sbox
};

const INV_SBOX: [u8; 256] = {
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inv
};

fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1B } else { 0 }
}

fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut out = 0;
    while b != 0 {
        if b & 1 != 0 {
            out ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    out
}

/// AES-128: the 11 round keys of one key.
#[derive(Clone)]
pub struct Aes128 {
    round_keys: [[u8; 16]; 11],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Self {
        let mut w = [[0u8; 4]; 44];
        for i in 0..4 {
            w[i].copy_from_slice(&key[i * 4..i * 4 + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            }
            for j in 0..4 {
                w[i][j] = w[i - 4][j] ^ t[j];
            }
        }
        let mut round_keys = [[0u8; 16]; 11];
        for (r, key) in round_keys.iter_mut().enumerate() {
            for c in 0..4 {
                key[c * 4..c * 4 + 4].copy_from_slice(&w[r * 4 + c]);
            }
        }
        Self { round_keys }
    }

    pub fn encrypt_block(&self, s: &mut [u8; 16]) {
        add(s, &self.round_keys[0]);
        for round in 1..11 {
            for b in s.iter_mut() {
                *b = SBOX[*b as usize];
            }
            shift_rows(s);
            if round != 10 {
                for c in 0..4 {
                    let col = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
                    let all = col[0] ^ col[1] ^ col[2] ^ col[3];
                    for r in 0..4 {
                        s[c * 4 + r] = col[r] ^ all ^ xtime(col[r] ^ col[(r + 1) % 4]);
                    }
                }
            }
            add(s, &self.round_keys[round]);
        }
    }

    pub fn decrypt_block(&self, s: &mut [u8; 16]) {
        add(s, &self.round_keys[10]);
        for round in (0..10).rev() {
            inv_shift_rows(s);
            for b in s.iter_mut() {
                *b = INV_SBOX[*b as usize];
            }
            add(s, &self.round_keys[round]);
            if round != 0 {
                for c in 0..4 {
                    let col = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
                    for r in 0..4 {
                        s[c * 4 + r] = mul(col[r], 14)
                            ^ mul(col[(r + 1) % 4], 11)
                            ^ mul(col[(r + 2) % 4], 13)
                            ^ mul(col[(r + 3) % 4], 9);
                    }
                }
            }
        }
    }
}

fn add(s: &mut [u8; 16], k: &[u8; 16]) {
    for (b, k) in s.iter_mut().zip(k) {
        *b ^= k;
    }
}

/// Row r moves r columns left; the state is column-major.
fn shift_rows(s: &mut [u8; 16]) {
    let t = *s;
    for c in 0..4 {
        for r in 0..4 {
            s[c * 4 + r] = t[((c + r) % 4) * 4 + r];
        }
    }
}

fn inv_shift_rows(s: &mut [u8; 16]) {
    let t = *s;
    for c in 0..4 {
        for r in 0..4 {
            s[((c + r) % 4) * 4 + r] = t[c * 4 + r];
        }
    }
}

/// `AesEncryption.encrypt` with `iv`: the IV, then the PKCS#5-padded CBC ciphertext.
pub fn cbc_encrypt(cipher: &Aes128, iv: [u8; 16], data: &[u8]) -> Vec<u8> {
    let pad = 16 - data.len() % 16;
    let mut out = Vec::with_capacity(16 + data.len() + pad);
    out.extend_from_slice(&iv);
    let mut chain = iv;
    let mut padded = data.to_vec();
    padded.resize(data.len() + pad, pad as u8);
    for block in padded.chunks_exact(16) {
        for (c, b) in chain.iter_mut().zip(block) {
            *c ^= b;
        }
        cipher.encrypt_block(&mut chain);
        out.extend_from_slice(&chain);
    }
    out
}

/// `AesEncryption.decrypt`: None when the length or the padding is wrong (a packet from an
/// old key, or not ours).
pub fn cbc_decrypt(cipher: &Aes128, data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 32 || data.len() % 16 != 0 {
        return None;
    }
    let mut chain: [u8; 16] = data[..16].try_into().ok()?;
    let mut out = Vec::with_capacity(data.len() - 16);
    for block in data[16..].chunks_exact(16) {
        let mut b: [u8; 16] = block.try_into().ok()?;
        cipher.decrypt_block(&mut b);
        for (x, c) in b.iter_mut().zip(&chain) {
            *x ^= c;
        }
        out.extend_from_slice(&b);
        chain = block.try_into().ok()?;
    }
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn fips_197_vector() {
        let key: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let aes = Aes128::new(&key);
        let mut block: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        aes.encrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex("69c4e0d86a7b0430d8cdb78070b4c55a"));
        aes.decrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex("00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn nist_cbc_vector() {
        // SP 800-38A F.2.1, first two blocks; the same IV then also pads a full block.
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let aes = Aes128::new(&key);
        let plain = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let sealed = cbc_encrypt(&aes, iv, &plain);
        assert_eq!(sealed[..16], iv);
        assert_eq!(sealed[16..48].to_vec(), hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2"));
        assert_eq!(sealed.len(), 16 + 48);
        assert_eq!(cbc_decrypt(&aes, &sealed).unwrap(), plain);
    }

    #[test]
    fn padding_is_checked() {
        let aes = Aes128::new(&[7; 16]);
        for n in [0, 1, 15, 16, 17, 100] {
            let data: Vec<u8> = (0..n as u8).collect();
            let sealed = cbc_encrypt(&aes, [9; 16], &data);
            assert_eq!(cbc_decrypt(&aes, &sealed).unwrap(), data);
        }
        let sealed = cbc_encrypt(&aes, [9; 16], b"hello");
        assert!(cbc_decrypt(&Aes128::new(&[8; 16]), &sealed).is_none());
        assert!(cbc_decrypt(&aes, &sealed[..31]).is_none());
    }
}
