//! Password-based encryption for the settings export/import file — see
//! triple-c#35.
//!
//! The exported payload can carry live credentials (the shared Claude OAuth
//! token, the gateway provider/master keys — see
//! `commands::settings_export_commands`), so this is not encryption for its
//! own sake; a wrong or missing key here is a real credential leak, not a
//! cosmetic bug. Argon2id derives a 256-bit key from the password (memory-
//! hard, meaningfully resistant to GPU/ASIC brute-forcing in a way PBKDF2 at
//! any reasonable iteration count is not), and AES-256-GCM is what actually
//! encrypts — authenticated, so a wrong password is detected by a failed tag
//! check rather than producing silent garbage.
//!
//! File format: `MAGIC (4 bytes) | salt (16 bytes) | nonce (12 bytes) |
//! ciphertext+tag`. The salt and nonce are not secret — they are written in
//! the clear right here, on purpose. The salt's only job is to make two
//! exports with the same password derive different keys (defeats a
//! precomputed-table attack against the password alone); the nonce's job is
//! GCM's requirement that a (key, nonce) pair never repeat. Both hold
//! because a fresh random value is drawn for each, on every call to
//! [`encrypt`].
//!
//! The whole header (magic + salt + nonce) is passed to AES-GCM as
//! associated data, not just placed alongside the ciphertext — free to do,
//! and it makes tampering with any header byte fail the same authentication
//! check the ciphertext gets, by construction rather than as a side effect
//! of the salt/nonce also feeding key derivation and the cipher.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use zeroize::Zeroizing;

/// Identifies the file as a Triple-C settings export and pins the format —
/// a change to the salt/nonce lengths or the KDF/cipher choice below needs a
/// new magic value, not a silent reinterpretation of old bytes.
const MAGIC: &[u8; 4] = b"TCX1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC.len() + SALT_LEN + NONCE_LEN;

/// Argon2id parameters: memory cost in KiB, time cost (iterations),
/// parallelism. `(19 MiB, 2, 1)` is OWASP's documented minimum recommendation
/// for Argon2id — deliberately heavier than a login-flow KDF would use, since
/// this runs once per export/import rather than on every request, so trading
/// roughly a second of wall time for real brute-force resistance costs
/// nothing a user would notice.
fn argon2_params() -> Params {
    Params::new(19 * 1024, 2, 1, Some(KEY_LEN)).expect("hardcoded Argon2 params are valid")
}

/// The derived key is wrapped in `Zeroizing` so it is overwritten with zeros
/// when it drops rather than left in freed memory for whatever reuses that
/// stack slot next — cheap insurance (`zeroize` is already in the dependency
/// tree via `aes-gcm`) for material that exists only to decrypt live
/// credentials.
fn derive_key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params());
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut *key)
        .map_err(|e| format!("Failed to derive encryption key: {}", e))?;
    Ok(key)
}

/// Encrypt `plaintext` with a key derived from `password`. Returns the whole
/// file's bytes (header + ciphertext) — see the module doc for the layout.
pub fn encrypt(plaintext: &[u8], password: &str) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT_LEN];
    rand::rng().fill_bytes(&mut salt);
    let key = derive_key(password, &salt)?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce_bytes);

    let cipher = Aes256Gcm::new_from_slice(&*key)
        .map_err(|e| format!("Failed to initialize cipher: {}", e))?;
    // The header (magic + salt + nonce) is authenticated as associated data
    // even though none of it is secret: it costs nothing extra here, and it
    // means tampering with any header byte is caught by the same tag check
    // that already covers the ciphertext, by construction rather than as a
    // side effect of the header also feeding key/nonce derivation.
    let ciphertext = cipher
        .encrypt(nonce, Payload { msg: plaintext, aad: &header })
        .map_err(|e| format!("Encryption failed: {}", e))?;

    let mut out = header;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a file produced by [`encrypt`]. The one error this returns for a
/// wrong password is deliberately generic ("wrong password, or the file is
/// corrupted") rather than distinguishing the two: GCM's authentication tag
/// fails to verify for the wrong key on essentially any ciphertext, so there
/// is no reliable way to tell "wrong password" from "corrupted file" apart,
/// and guessing would be worse than saying so.
///
/// Returns `Zeroizing<Vec<u8>>` rather than a plain `Vec<u8>` — the plaintext
/// this recovers is the whole settings-plus-secrets payload, so it gets the
/// same "wipe it when it drops" treatment as the derived key in
/// [`derive_key`].
pub fn decrypt(data: &[u8], password: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    if data.len() < HEADER_LEN {
        return Err("This does not look like a Triple-C settings export (file too short).".to_string());
    }
    if &data[..MAGIC.len()] != MAGIC {
        return Err("This does not look like a Triple-C settings export (unrecognized file).".to_string());
    }
    let header = &data[..HEADER_LEN];
    let salt = &data[MAGIC.len()..MAGIC.len() + SALT_LEN];
    let nonce_bytes = &data[MAGIC.len() + SALT_LEN..HEADER_LEN];
    let ciphertext = &data[HEADER_LEN..];

    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&*key)
        .map_err(|e| format!("Failed to initialize cipher: {}", e))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, Payload { msg: ciphertext, aad: header })
        .map(Zeroizing::new)
        .map_err(|_| "Wrong password, or the file is corrupted.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_with_the_right_password_recovers_the_plaintext() {
        let plaintext = b"{\"settings\": \"whatever\"}";
        let encrypted = encrypt(plaintext, "correct horse battery staple").unwrap();
        let decrypted = decrypt(&encrypted, "correct horse battery staple").unwrap();
        assert_eq!(&*decrypted, plaintext);
    }

    #[test]
    fn the_wrong_password_fails_rather_than_returning_garbage() {
        let encrypted = encrypt(b"secret payload", "correct password").unwrap();
        let result = decrypt(&encrypted, "wrong password");
        assert!(result.is_err(), "decrypting with the wrong password must fail, not silently succeed");
    }

    #[test]
    fn two_exports_of_the_same_plaintext_and_password_produce_different_files() {
        // If this ever failed it would mean the salt or nonce stopped being
        // randomized — either one repeating is a real security regression
        // (a fixed salt lets an attacker precompute against the password
        // alone; a repeated (key, nonce) pair breaks GCM's guarantees
        // outright), not just a cosmetic one.
        let a = encrypt(b"same plaintext", "same password").unwrap();
        let b = encrypt(b"same plaintext", "same password").unwrap();
        assert_ne!(a, b, "two independent exports must not be byte-identical");
    }

    #[test]
    fn corrupting_a_single_byte_of_ciphertext_is_detected() {
        let mut encrypted = encrypt(b"tamper-evident payload", "a password").unwrap();
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0xFF;
        assert!(decrypt(&encrypted, "a password").is_err());
    }

    #[test]
    fn a_file_that_is_too_short_is_rejected_cleanly_not_by_panicking() {
        assert!(decrypt(b"short", "any password").is_err());
        assert!(decrypt(b"", "any password").is_err());
    }

    #[test]
    fn a_file_with_the_wrong_magic_is_rejected() {
        let mut encrypted = encrypt(b"payload", "password").unwrap();
        encrypted[0] = b'X';
        assert!(decrypt(&encrypted, "password").is_err());
    }
}
