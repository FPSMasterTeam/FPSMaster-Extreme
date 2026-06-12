/// AES-128-CFB8 stream cipher for Minecraft 1.8 online-mode encryption.
///
/// After `EncryptionResponse`, every byte read from or written to the TCP
/// stream is encrypted/decrypted with AES-128-CFB8 (key == IV == shared secret).
///
/// The `cfb8` crate's `AsyncStreamCipher` takes `self` by value (it is not
/// designed for use through a `&mut` reference).  We therefore maintain the
/// shift register (the 16-byte IV) ourselves and call the raw AES block cipher
/// one block at a time — identical to what every other Minecraft Rust client
/// does.
use aes::{
    cipher::{BlockEncrypt, KeyInit},
    Aes128,
};

/// Stateful AES-128-CFB8 cipher (used for both encrypting and decrypting; the
/// encrypt direction feeds the output byte into the shift register while the
/// decrypt direction feeds the input byte in).
pub struct Aes128Cfb8 {
    cipher: Aes128,
    /// Current shift register (updated after each byte).
    sr: [u8; 16],
    /// Direction: `true` = encrypt (feed ciphertext into SR), `false` = decrypt
    /// (feed plaintext into SR — but in CFB-8 the only difference is which byte
    /// you shift in, so both directions use the same block-encrypt step).
    decrypt: bool,
}

impl Aes128Cfb8 {
    /// Create an encryptor (key == IV == `secret`).
    pub fn encryptor(secret: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(secret.into()),
            sr: *secret,
            decrypt: false,
        }
    }

    /// Create a decryptor (key == IV == `secret`).
    pub fn decryptor(secret: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(secret.into()),
            sr: *secret,
            decrypt: true,
        }
    }

    /// Encrypt or decrypt a single byte in-place (depending on construction).
    #[inline]
    pub fn process_byte(&mut self, byte: &mut u8) {
        // Encrypt the shift register to get the key stream byte.
        let mut block = aes::Block::from(self.sr);
        self.cipher.encrypt_block(&mut block);
        let ks = block[0];

        let original = *byte;
        *byte ^= ks;

        // Shift the register left by one byte and push the appropriate byte.
        // Encrypt: shift in the ciphertext (= *byte after XOR).
        // Decrypt: shift in the original plaintext (= original).
        let shift_in = if self.decrypt { original } else { *byte };
        self.sr.rotate_left(1);
        self.sr[15] = shift_in;
    }

    /// Process an entire buffer in-place.
    pub fn process(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            self.process_byte(b);
        }
    }
}
