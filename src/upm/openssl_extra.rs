//! The rust openssl crate does not wrap every possible function that is provided by OpenSSL.  We
//! must add our own wrapper for PKCS12_key_gen_uni() so we can perform the key generation
//! algorithm that is required by the UPMv3 format.

extern crate libc;
extern crate openssl;
extern crate openssl_sys as ffi;

use self::libc::{c_int, c_uchar};

/// An error with lib `ERR_LIB_EVP` indicates the error was returned from an OpenSSL EVP function.
static ERR_LIB_EVP: u8 = 6;

/// An error with this reason code indicates a decryption failure, which usually means that the
/// provided password was incorrect.
static EVP_R_BAD_DECRYPT: u16 = 100;

/// Decompose an error code into a 3-tuple containing the library, function, and reason codes.
fn decompose_error_code(code: u32) -> (u8, u16, u16) {
    (
        (code >> 24 & 0xFF) as u8,
        (code >> 12 & 0xFFF) as u16,
        (code & 0xFFF) as u16,
    )
}

/// Return true if the provided OpenSSL error stack contains any EVP "bad decrypt" error, which
/// usually means that the provided password was incorrect.  Unfortunately, the converse is not
/// necessarily the case -- a bad password can sometimes return gibberish plaintext without
/// indicating EVP_R_BAD_DECRYPT.  (The UPM format doesn't allow for any sort of authentication or
/// validity checking.)
pub fn is_bad_decrypt(error_stack: &openssl::error::ErrorStack) -> bool {
    for e in error_stack.errors() {
        let (lib, _, reason) = decompose_error_code(e.code() as u32);
        if lib == ERR_LIB_EVP && reason == EVP_R_BAD_DECRYPT {
            return true;
        }
    }
    false
}

extern "C" {
    /// This is the OpenSSL C function which performs PKCS#12 key derivation to generate a key or
    /// IV based on the provided UCS-2BE password string.
    ///
    /// Newer versions of OpenSSL have a PKCS12_key_gen_utf8 function that takes a UTF-8 string.
    /// That function would have been better to use, but since it was only recently added we can't
    /// count on it being available.  It's only a thin wrapper around PKCS12_key_gen_uni(), anyway.
    pub fn PKCS12_key_gen_uni(
        pass: *const c_uchar,
        passlen: c_int,
        salt: *const c_uchar,
        saltlen: c_int,
        id: c_int,
        iter: c_int,
        n: c_int,
        out: *mut c_uchar,
        md_type: *const ffi::EVP_MD,
    ) -> c_int;
}

/// Convert a UTF-8 encoded string into a UCS-2BE encoding suitable for PKCS12_key_gen_uni().
///
/// PKCS#12 wants strings in "BMPString" encoding, which is actually UCS-2BE.  (Not "UTF-16" as the
/// OpenSSL comments would lead you to believe.)  This only allows for codepoints in the Basic
/// Multilingual Plane.  Hopefully nobody is using fancy emojis in their passwords.
fn str_to_bmpstring(text: &str) -> Box<[u8]> {
    // Use a boxed slice so the sensitive data can be reliably zeroed later.
    // (A Vec may reallocate and leave behind sensitive material.)
    let final_length = text.chars().count() * 2 + 2;
    let mut bmpstring: Box<[u8]> = vec![0; final_length].into_boxed_slice();

    let mut index = 0;
    for c in text.chars() {
        let codepoint = c as u32;
        // The upper 16 bits of the codepoint will be discarded.
        bmpstring[index] = ((codepoint >> 8) & 0xFF) as u8;
        index += 1;
        bmpstring[index] = ((codepoint >> 0) & 0xFF) as u8;
        index += 1;
    }
    bmpstring[index] = 0;
    index += 1;
    bmpstring[index] = 0;

    bmpstring
}

/// Generate a key or IV using the key derivation function specified in RFC 7292, "PKCS #12:
/// Personal Information Exchange Syntax v1.1", Appendix B, "Deriving Keys and IVs from Passwords
/// and Salt".
pub fn pkcs12_key_gen(
    pass: &str,
    salt: &[u8],
    id: u8,
    iter: usize,
    key: &mut [u8],
    hash: openssl::hash::MessageDigest,
) -> Result<(), openssl::error::ErrorStack> {

    // Convert password to a BMPString
    let mut pass = str_to_bmpstring(pass);

    // Proxy to OpenSSL's PKCS12_key_gen_uni().
    let result: c_int;
    unsafe {
        assert!(pass.len() <= c_int::max_value() as usize);
        assert!(salt.len() <= c_int::max_value() as usize);
        assert!(key.len() <= c_int::max_value() as usize);
        ffi::init();
        result = PKCS12_key_gen_uni(
            pass.as_ptr() as *const _,
            pass.len() as c_int,
            salt.as_ptr(),
            salt.len() as c_int,
            id as c_int,
            iter as c_int,
            key.len() as c_int,
            key.as_mut_ptr(),
            hash.as_ptr(),
        );
    }

    // Zero the encoded bmpstring.
    // This may need to be revisited -- will the compiler optimize this out?
    // Best practices for sensitive material in Rust are still evolving.
    for i in 0..pass.len() {
        pass[i] = 0;
    }

    if result <= 0 {
        Err(openssl::error::ErrorStack::get())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decompose_error_code() {
        assert_eq!(decompose_error_code(0x06000064), (
            ERR_LIB_EVP,
            0,
            EVP_R_BAD_DECRYPT,
        ));
        assert_eq!(decompose_error_code(0x12345678), (0x12, 0x345, 0x678));
        assert_eq!(decompose_error_code(0x00000000), (0x00, 0x000, 0x000));
        assert_eq!(decompose_error_code(0xFFFFFFFF), (0xFF, 0xFFF, 0xFFF));
    }

    const HELLOWORLD_STR: &str = "hello world";
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const FANCY_UTF8: &[u8] = &[
        0xCE, 0xB3, 0xCE, 0xBB, 0xCF, 0x8E, 0xCF, 0x83,
        0xCF, 0x83, 0xCE, 0xB1
    ];
    const EMPTY_BMPSTRING: &[u8] = &[0x00, 0x00];
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const HELLOWORLD_BMPSTRING: &[u8] = &[
        0x00, 0x68, 0x00, 0x65, 0x00, 0x6C, 0x00, 0x6C,
        0x00, 0x6F, 0x00, 0x20, 0x00, 0x77, 0x00, 0x6F,
        0x00, 0x72, 0x00, 0x6C, 0x00, 0x64, 0x00, 0x00
    ];
    #[cfg_attr(rustfmt, rustfmt_skip)]
    const FANCY_BMPSTRING: &[u8] = &[
        0x03, 0xB3, 0x03, 0xBB, 0x03, 0xCE, 0x03, 0xC3,
        0x03, 0xC3, 0x03, 0xB1, 0x00, 0x00
    ];

    #[test]
    fn test_str_to_bmpstring() {
        use std::str;
        assert_eq!(&*str_to_bmpstring(""), EMPTY_BMPSTRING);
        assert_eq!(&*str_to_bmpstring(HELLOWORLD_STR), HELLOWORLD_BMPSTRING);
        assert_eq!(
            &*str_to_bmpstring(str::from_utf8(FANCY_UTF8).unwrap()),
            FANCY_BMPSTRING
        );
    }
}
