//! Perform encryption and decryption operations to support the ciphertext component of UPMv3
//! database files.
//!
//! This module implements the following by way of OpenSSL:
//!
//! 1. The UPMv3 key derivation function (KDF) to convert a password into a private key.
//! 2. Encryption.
//! 3. Decryption.
//!
//! UPM encrypts databases using an AES 256-bit cipher in CBC mode.  The private key is derived
//! from a password using a PKCS#12 key derivation function (KDF) as specified in RFC 7292 Appendix
//! B, using 20 iterations.  This KDF is likely the weakest point of UPM's crypto for the following
//! reasons:
//!
//! 1. This algorithm has been deprecated and is not recommended for new use.
//! 2. An iteration count of 20 likely falls short of the ideal by a couple orders of magnitude.
//!
//! Nonetheless, use of this KDF is required to interoperate with UPMv3 databases.
//!

extern crate openssl;

use openssl_extra;
use error::UpmError;

const KEY_MATERIAL_ID: u8 = 1;
const IV_MATERIAL_ID: u8 = 2;
const KEY_MATERIAL_BITS: usize = 256;
const IV_MATERIAL_BITS: usize = 128;
const KEY_MATERIAL_SIZE: usize = KEY_MATERIAL_BITS / 8;
const IV_MATERIAL_SIZE: usize = IV_MATERIAL_BITS / 8;
const KEY_DERIVATION_ITERATIONS: usize = 20;

/// This KeyIVPair struct is to arrange zeroing of the key and IV buffers when they go out of
/// scope.  Note that the current zeroing method is probably naive, and may not survive compiler
/// optimization.  The best practices in Rust for storing sensitive material are still being worked
/// out.
///
/// The following GitHub issue is informative:
///
/// * https://github.com/isislovecruft/curve25519-dalek/issues/11
///
/// Note that there is more sensitive data than just the key/IV.  In particular, the following
/// items are sensitive and we need to develop a post-zeroing solution for them:
///
/// 1. The master password.
/// 2. The account records, including their respective managed passwords.
/// 3. Any intermediate data buffers used to pass these items around.
///
/// We should probably consider using one of these tools:
///
/// * https://github.com/cesarb/clear_on_drop
/// * https://github.com/ticki/secbox
/// * https://github.com/stouset/secrets
/// * https://github.com/myfreeweb/secstr
struct KeyIVPair {
    pub key: [u8; KEY_MATERIAL_SIZE],
    pub iv: [u8; IV_MATERIAL_SIZE],
}

impl Drop for KeyIVPair {
    fn drop(&mut self) {
        for i in 0..self.key.len() {
            self.key[i] = 0;
        }
        for i in 0..self.iv.len() {
            self.iv[i] = 0;
        }
    }
}

impl KeyIVPair {
    pub fn new() -> KeyIVPair {
        KeyIVPair {
            key: [0u8; KEY_MATERIAL_SIZE],
            iv: [0u8; IV_MATERIAL_SIZE],
        }
    }
}

/// Perform key and IV generation based on the algorithm specified here:
///
/// * RFC 7292: PKCS #12: Personal Information Exchange Syntax v1.1 Appendix B.  Deriving Keys and
/// IVs from Passwords and Salt
///
/// Note that this is probably the weak point of UPM crypto for the reasons mentioned above.
fn pkcs12_derive_key(password: &str, salt: &[u8], pair: &mut KeyIVPair) -> Result<(), UpmError> {
    match openssl_extra::pkcs12_key_gen(
        password,
        &salt,
        KEY_MATERIAL_ID,
        KEY_DERIVATION_ITERATIONS,
        &mut pair.key,
        openssl::hash::MessageDigest::sha256(),
    ) {
        Ok(()) => {}
        Err(_) => {
            return Err(UpmError::KeyIVGeneration);
        }
    };
    match openssl_extra::pkcs12_key_gen(
        password,
        &salt,
        IV_MATERIAL_ID,
        KEY_DERIVATION_ITERATIONS,
        &mut pair.iv,
        openssl::hash::MessageDigest::sha256(),
    ) {
        Ok(()) => {}
        Err(_) => {
            return Err(UpmError::KeyIVGeneration);
        }
    };
    Ok(())
}

/// Decrypt the UPMv3 database ciphertext using the provided password and salt.
pub fn decrypt(ciphertext: &[u8], password: &str, salt: &[u8]) -> Result<Vec<u8>, UpmError> {
    let mut pair = KeyIVPair::new();
    try!(pkcs12_derive_key(password, salt, &mut pair));

    match openssl::symm::decrypt(
        openssl::symm::Cipher::aes_256_cbc(),
        &pair.key[..],
        Option::Some(&pair.iv[..]),
        &ciphertext[..],
    ) {
        Ok(x) => Ok(x),
        Err(error_stack) => {
            if openssl_extra::is_bad_decrypt(&error_stack) {
                Err(UpmError::BadPassword)
            } else {
                Err(From::from(error_stack))
            }
        }
    }
}

/// Encrypt the UPMv3 database plaintext using the provided password and salt.
pub fn encrypt(plaintext: &[u8], password: &str, salt: &[u8]) -> Result<Vec<u8>, UpmError> {
    let mut pair = KeyIVPair::new();
    try!(pkcs12_derive_key(password, salt, &mut pair));

    match openssl::symm::encrypt(
        openssl::symm::Cipher::aes_256_cbc(),
        &pair.key[..],
        Option::Some(&pair.iv[..]),
        &plaintext[..],
    ) {
        Ok(x) => Ok(x),
        Err(error_stack) => {
            if openssl_extra::is_bad_decrypt(&error_stack) {
                Err(UpmError::BadPassword)
            } else {
                Err(From::from(error_stack))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "xyzzy";
    const SALT: &[u8] = &[0x35, 0xB3, 0x66, 0xE2, 0xF5, 0x28, 0xBF, 0x3E];
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const EXPECTED_KEY: &[u8] = &[
        0x8D, 0x17, 0x3A, 0x33, 0x4D, 0xE4, 0xD4, 0x1F,
        0x75, 0x6A, 0x3C, 0xEB, 0x74, 0xE0, 0x9E, 0xC4,
        0xEC, 0x8F, 0xD3, 0x83, 0x3F, 0x15, 0xAF, 0x86,
        0x54, 0xFE, 0x77, 0x37, 0x32, 0x9E, 0x50, 0x10,
    ];
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const EXPECTED_IV: &[u8] = &[
        0x37, 0x26, 0x45, 0x5F, 0xA5, 0x33, 0x0D, 0xD1,
        0x53, 0x78, 0x3A, 0x75, 0x56, 0xB9, 0x34, 0xE3,
    ];
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const CIPHERTEXT: &[u8] = &[
        0x0E, 0xF5, 0x4D, 0xD8, 0x47, 0x6B, 0xC2, 0x4E,
        0xA0, 0xA0, 0x47, 0x02, 0x20, 0x25, 0xD8, 0xDB,
        0x01, 0x41, 0xB2, 0x06, 0xE2, 0xB1, 0x50, 0x93,
        0xC1, 0x26, 0x01, 0xE9, 0xA0, 0x96, 0xFA, 0xC7,
        0x0B, 0xE7, 0x80, 0x4F, 0x05, 0x4E, 0xE7, 0x76,
        0x4F, 0xC3, 0x42, 0xAC, 0x76, 0x81, 0x27, 0x8B,
    ];
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const PLAINTEXT: &[u8] = &[
        0x30, 0x30, 0x30, 0x31, 0x31, 0x30, 0x30, 0x30,
        0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30,
        0x34, 0x61, 0x63, 0x63, 0x74, 0x30, 0x30, 0x30,
        0x34, 0x75, 0x73, 0x65, 0x72, 0x30, 0x30, 0x30,
        0x34, 0x70, 0x61, 0x73, 0x73, 0x30, 0x30, 0x30,
        0x30, 0x30, 0x30, 0x30, 0x30,
    ];

    #[test]
    fn test_pkcs12_derive_key() {
        let mut pair = KeyIVPair::new();
        let result = pkcs12_derive_key(PASSWORD, SALT, &mut pair);
        assert_matches!(result, Ok(_));
        assert_eq!(pair.key, EXPECTED_KEY);
        assert_eq!(pair.iv, EXPECTED_IV);
    }

    #[test]
    fn test_decrypt() {
        let result = decrypt(CIPHERTEXT, PASSWORD, SALT);
        assert_matches!(result, Ok(_));
        assert_eq!(result.unwrap().as_slice(), PLAINTEXT);
    }

    #[test]
    fn test_encrypt() {
        let result = encrypt(PLAINTEXT, PASSWORD, SALT);
        assert_matches!(result, Ok(_));
        assert_eq!(result.unwrap().as_slice(), CIPHERTEXT);
    }
}
