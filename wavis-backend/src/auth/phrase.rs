//! Argon2id phrase hashing, verification, and AES-256-GCM encryption at rest.
//!
//! This module is the ONLY place where phrase encryption/decryption happens.
//! The database always stores ciphertext blobs — never plaintext salt or verifier bytes.

/// Argon2id configuration for phrase hashing.
#[derive(Debug, Clone)]
pub struct PhraseConfig {
    /// Memory cost in KiB (default: 65536 = 64 MiB).
    pub memory_cost_kib: u32,
    /// Number of iterations (default: 3).
    pub iterations: u32,
    /// Degree of parallelism (default: 1).
    pub parallelism: u32,
}

impl Default for PhraseConfig {
    fn default() -> Self {
        Self {
            memory_cost_kib: 65536,
            iterations: 3,
            parallelism: 1,
        }
    }
}

/// Result of hashing a phrase with Argon2id.
#[derive(Debug)]
pub struct PhraseHash {
    /// Random salt (16 bytes, CSPRNG).
    pub salt: Vec<u8>,
    /// Argon2id output (verifier).
    pub verifier: Vec<u8>,
}

/// Pre-computed dummy verifier for timing equalization on unknown Recovery IDs.
/// Generated at startup, lives in memory only (never persisted to DB).
#[derive(Debug)]
pub struct DummyVerifier {
    /// Random salt used for the dummy hash.
    pub salt: Vec<u8>,
    /// Argon2id output for the dummy hash.
    pub verifier: Vec<u8>,
}

/// Errors from phrase hashing, verification, and encryption operations.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum PhraseError {
    #[error("phrase hashing failed: {0}")]
    HashingFailed(String),
    #[error("phrase verification failed")]
    VerificationFailed,
    #[error("invalid phrase config: {0}")]
    InvalidConfig(String),
    #[error("encryption failed: {0}")]
    EncryptionFailed(String),
    #[error("decryption failed: {0}")]
    DecryptionFailed(String),
}

use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use uuid::Uuid;
use zeroize::Zeroize;

/// Constant-time byte comparison to prevent timing side-channels.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Build Argon2id hasher from config.
fn build_argon2(config: &PhraseConfig) -> Result<Argon2<'_>, PhraseError> {
    let params = Params::new(
        config.memory_cost_kib,
        config.iterations,
        config.parallelism,
        Some(32),
    )
    .map_err(|e| PhraseError::InvalidConfig(e.to_string()))?;

    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Build the user-bound salt: `random_salt || user_id.as_bytes()`.
/// This ensures two users with the same phrase produce different verifiers.
fn build_bound_salt(random_salt: &[u8], user_id: &Uuid) -> Vec<u8> {
    let mut bound_salt = Vec::with_capacity(random_salt.len() + 16);
    bound_salt.extend_from_slice(random_salt);
    bound_salt.extend_from_slice(user_id.as_bytes());
    bound_salt
}

/// Hash a phrase with a fresh random salt, bound to the given user_id.
///
/// The Argon2id salt input is derived as `random_salt || user_id.as_bytes()` so that
/// two users with the same phrase produce different verifiers (prevents cross-account
/// verifier reuse).
///
/// Returns a `PhraseHash` containing the 16-byte random salt (NOT the concatenated salt)
/// and the 32-byte Argon2id verifier output.
pub fn hash_phrase(
    phrase: &str,
    user_id: &Uuid,
    config: &PhraseConfig,
) -> Result<PhraseHash, PhraseError> {
    let argon2 = build_argon2(config)?;

    // Generate 16-byte CSPRNG salt.
    let mut random_salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random_salt);

    // Build user-bound salt: random_salt || user_id bytes.
    let bound_salt = build_bound_salt(&random_salt, user_id);

    // Make a mutable copy of the phrase so we can zeroize it after hashing.
    let mut phrase_bytes = phrase.as_bytes().to_vec();

    // Hash the phrase.
    let mut verifier = [0u8; 32];
    let result = argon2
        .hash_password_into(&phrase_bytes, &bound_salt, &mut verifier)
        .map_err(|e| PhraseError::HashingFailed(e.to_string()));

    // Best-effort: zeroize the mutable phrase copy.
    phrase_bytes.zeroize();

    result?;

    Ok(PhraseHash {
        salt: random_salt.to_vec(),
        verifier: verifier.to_vec(),
    })
}

/// Verify a phrase against stored salt + verifier, using the same `salt || user_id` binding.
///
/// Reconstructs the Argon2id parameters with the same user-bound salt, hashes the
/// candidate phrase, and compares the result against the stored verifier using
/// constant-time comparison.
pub fn verify_phrase(
    phrase: &str,
    user_id: &Uuid,
    salt: &[u8],
    verifier: &[u8],
    config: &PhraseConfig,
) -> Result<bool, PhraseError> {
    let argon2 = build_argon2(config)?;

    // Reconstruct the user-bound salt.
    let bound_salt = build_bound_salt(salt, user_id);

    // Make a mutable copy of the phrase so we can zeroize it after hashing.
    let mut phrase_bytes = phrase.as_bytes().to_vec();

    // Hash the candidate phrase with the same parameters.
    let mut candidate_verifier = [0u8; 32];
    let result = argon2
        .hash_password_into(&phrase_bytes, &bound_salt, &mut candidate_verifier)
        .map_err(|e| PhraseError::HashingFailed(e.to_string()));

    // Best-effort: zeroize the mutable phrase copy.
    phrase_bytes.zeroize();

    result?;

    // Constant-time comparison to prevent timing side-channels.
    Ok(constant_time_eq(&candidate_verifier, verifier))
}

/// Generate a dummy verifier at startup for timing equalization.
/// Uses a dummy UUID (`Uuid::nil()`) for the user_id binding. Lives in memory only (not from DB).
/// A random 32-byte phrase is generated internally so the verifier is unpredictable.
pub fn generate_dummy_verifier(config: &PhraseConfig) -> DummyVerifier {
    let dummy_user_id = Uuid::nil();

    // Generate a random 32-byte "phrase" that nobody will ever know.
    let mut random_phrase_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut random_phrase_bytes);
    // Convert to hex string so it's valid UTF-8 for hash_phrase.
    let random_phrase = hex::encode(random_phrase_bytes);

    let hash = hash_phrase(&random_phrase, &dummy_user_id, config)
        .expect("dummy verifier generation must not fail with valid config");

    DummyVerifier {
        salt: hash.salt,
        verifier: hash.verifier,
    }
}

/// Verify against the dummy verifier (always returns false, but takes same time).
/// Uses the same dummy UUID (`Uuid::nil()`) for the user_id binding. In-memory only.
/// This prevents timing oracles on unknown Recovery IDs.
pub fn verify_dummy(
    phrase: &str,
    dummy: &DummyVerifier,
    config: &PhraseConfig,
) -> Result<bool, PhraseError> {
    verify_phrase(phrase, &Uuid::nil(), &dummy.salt, &dummy.verifier, config)
}

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};

/// AES-256-GCM nonce size in bytes.
const AES_GCM_NONCE_LEN: usize = 12;
/// Required key length for AES-256-GCM.
const AES_256_KEY_LEN: usize = 32;

/// Encrypt a single blob with AES-256-GCM.
/// Output layout: `nonce (12 bytes) || ciphertext || tag (16 bytes)`.
/// A fresh random nonce is generated for each call.
fn encrypt_blob(plaintext: &[u8], encryption_key: &[u8]) -> Result<Vec<u8>, PhraseError> {
    if encryption_key.len() != AES_256_KEY_LEN {
        return Err(PhraseError::EncryptionFailed(format!(
            "key must be exactly {} bytes, got {}",
            AES_256_KEY_LEN,
            encryption_key.len()
        )));
    }

    let cipher = Aes256Gcm::new_from_slice(encryption_key)
        .map_err(|e| PhraseError::EncryptionFailed(e.to_string()))?;

    let mut nonce_bytes = [0u8; AES_GCM_NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| PhraseError::EncryptionFailed(e.to_string()))?;

    // nonce || ciphertext (which includes the 16-byte tag appended by aes-gcm)
    let mut out = Vec::with_capacity(AES_GCM_NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a single blob produced by `encrypt_blob`.
/// Expects input layout: `nonce (12 bytes) || ciphertext || tag (16 bytes)`.
fn decrypt_blob(encrypted: &[u8], encryption_key: &[u8]) -> Result<Vec<u8>, PhraseError> {
    if encryption_key.len() != AES_256_KEY_LEN {
        return Err(PhraseError::DecryptionFailed(format!(
            "key must be exactly {} bytes, got {}",
            AES_256_KEY_LEN,
            encryption_key.len()
        )));
    }

    // Minimum length: 12-byte nonce + 16-byte tag = 28 bytes (empty plaintext)
    if encrypted.len() < AES_GCM_NONCE_LEN + 16 {
        return Err(PhraseError::DecryptionFailed(
            "ciphertext too short".to_string(),
        ));
    }

    let (nonce_bytes, ciphertext) = encrypted.split_at(AES_GCM_NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher = Aes256Gcm::new_from_slice(encryption_key)
        .map_err(|e| PhraseError::DecryptionFailed(e.to_string()))?;

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| PhraseError::DecryptionFailed(e.to_string()))
}

/// Encrypt phrase_salt and phrase_verifier with AES-256-GCM before DB write.
/// Each encrypted blob is: nonce (12 bytes) || ciphertext || tag (16 bytes).
/// A fresh random nonce is generated for each encryption call.
pub fn encrypt_phrase_data(
    salt: &[u8],
    verifier: &[u8],
    encryption_key: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), PhraseError> {
    let encrypted_salt = encrypt_blob(salt, encryption_key)?;
    let encrypted_verifier = encrypt_blob(verifier, encryption_key)?;
    Ok((encrypted_salt, encrypted_verifier))
}

/// Decrypt phrase_salt and phrase_verifier after DB read.
pub fn decrypt_phrase_data(
    encrypted_salt: &[u8],
    encrypted_verifier: &[u8],
    encryption_key: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), PhraseError> {
    let salt = decrypt_blob(encrypted_salt, encryption_key)?;
    let verifier = decrypt_blob(encrypted_verifier, encryption_key)?;
    Ok((salt, verifier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Low-cost Argon2id config for fast property tests.
    fn test_config() -> PhraseConfig {
        PhraseConfig {
            memory_cost_kib: 256,
            iterations: 1,
            parallelism: 1,
        }
    }

    // Feature: user-identity-recovery, Property 4: Phrase hash/verify round-trip
    // **Validates: Requirements 4.2, 4.3, 4.7, 5.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_phrase_hash_verify_roundtrip(
            phrase in "[a-zA-Z0-9]{1,64}",
            uuid_bytes in prop::array::uniform16(any::<u8>()),
        ) {
            let config = test_config();
            let user_id = Uuid::from_bytes(uuid_bytes);

            let hash = hash_phrase(&phrase, &user_id, &config).unwrap();

            // Same phrase + same user_id → verify succeeds
            let result = verify_phrase(&phrase, &user_id, &hash.salt, &hash.verifier, &config).unwrap();
            prop_assert!(result, "verify_phrase should return true for the same phrase and user_id");
        }

        #[test]
        fn prop_phrase_wrong_phrase_fails(
            phrase in "[a-zA-Z0-9]{1,64}",
            wrong_phrase in "[a-zA-Z0-9]{1,64}",
            uuid_bytes in prop::array::uniform16(any::<u8>()),
        ) {
            let config = test_config();
            let user_id = Uuid::from_bytes(uuid_bytes);

            // Skip when phrases happen to be equal
            prop_assume!(phrase != wrong_phrase);

            let hash = hash_phrase(&phrase, &user_id, &config).unwrap();

            // Different phrase → verify fails
            let result = verify_phrase(&wrong_phrase, &user_id, &hash.salt, &hash.verifier, &config).unwrap();
            prop_assert!(!result, "verify_phrase should return false for a different phrase");
        }

        #[test]
        fn prop_phrase_wrong_user_id_fails(
            phrase in "[a-zA-Z0-9]{1,64}",
            uuid_bytes_a in prop::array::uniform16(any::<u8>()),
            uuid_bytes_b in prop::array::uniform16(any::<u8>()),
        ) {
            let config = test_config();
            let user_id_a = Uuid::from_bytes(uuid_bytes_a);
            let user_id_b = Uuid::from_bytes(uuid_bytes_b);

            // Skip when user_ids happen to be equal
            prop_assume!(user_id_a != user_id_b);

            let hash = hash_phrase(&phrase, &user_id_a, &config).unwrap();

            // Same phrase but different user_id → verify fails
            let result = verify_phrase(&phrase, &user_id_b, &hash.salt, &hash.verifier, &config).unwrap();
            prop_assert!(!result, "verify_phrase should return false for a different user_id");
        }
    }

    // Feature: user-identity-recovery, Property 19: Phrase hash output validity
    // **Validates: Requirements 4.2, 4.3, 4.7**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20))]

        #[test]
        fn prop_phrase_hash_salt_is_16_bytes(
            phrase in "[a-zA-Z0-9]{1,64}",
            uuid_bytes in prop::array::uniform16(any::<u8>()),
        ) {
            let config = test_config();
            let user_id = Uuid::from_bytes(uuid_bytes);

            let hash = hash_phrase(&phrase, &user_id, &config).unwrap();

            prop_assert_eq!(hash.salt.len(), 16, "salt must be exactly 16 bytes");
        }

        #[test]
        fn prop_phrase_hash_verifier_is_32_bytes(
            phrase in "[a-zA-Z0-9]{1,64}",
            uuid_bytes in prop::array::uniform16(any::<u8>()),
        ) {
            let config = test_config();
            let user_id = Uuid::from_bytes(uuid_bytes);

            let hash = hash_phrase(&phrase, &user_id, &config).unwrap();

            prop_assert_eq!(hash.verifier.len(), 32, "verifier must be exactly 32 bytes");
            prop_assert!(hash.verifier.iter().any(|&b| b != 0), "verifier must be non-empty (not all zeros)");
        }

        #[test]
        fn prop_phrase_hash_different_salts_per_call(
            phrase in "[a-zA-Z0-9]{1,64}",
            uuid_bytes in prop::array::uniform16(any::<u8>()),
        ) {
            let config = test_config();
            let user_id = Uuid::from_bytes(uuid_bytes);

            let hash1 = hash_phrase(&phrase, &user_id, &config).unwrap();
            let hash2 = hash_phrase(&phrase, &user_id, &config).unwrap();

            // Random salts should differ between calls
            prop_assert_ne!(hash1.salt, hash2.salt, "two calls should produce different random salts");
        }

        #[test]
        fn prop_phrase_hash_different_verifiers_for_different_user_ids(
            phrase in "[a-zA-Z0-9]{1,64}",
            uuid_bytes_a in prop::array::uniform16(any::<u8>()),
            uuid_bytes_b in prop::array::uniform16(any::<u8>()),
        ) {
            let config = test_config();
            let user_id_a = Uuid::from_bytes(uuid_bytes_a);
            let user_id_b = Uuid::from_bytes(uuid_bytes_b);

            prop_assume!(user_id_a != user_id_b);

            let hash_a = hash_phrase(&phrase, &user_id_a, &config).unwrap();
            let hash_b = hash_phrase(&phrase, &user_id_b, &config).unwrap();

            // Same phrase but different user_ids → different verifiers (user-bound hashing)
            prop_assert_ne!(hash_a.verifier, hash_b.verifier,
                "same phrase with different user_ids must produce different verifiers");
        }
    }

    // Feature: user-identity-recovery, Property 23: Phrase encryption round-trip
    // **Validates: Requirements 4.9**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_encrypt_decrypt_roundtrip(
            salt in prop::collection::vec(any::<u8>(), 16),
            verifier in prop::collection::vec(any::<u8>(), 32),
            key in prop::collection::vec(any::<u8>(), 32),
        ) {
            let (enc_salt, enc_verifier) = encrypt_phrase_data(&salt, &verifier, &key).unwrap();

            let (dec_salt, dec_verifier) = decrypt_phrase_data(&enc_salt, &enc_verifier, &key).unwrap();

            prop_assert_eq!(&dec_salt, &salt, "decrypted salt must match original");
            prop_assert_eq!(&dec_verifier, &verifier, "decrypted verifier must match original");
        }

        #[test]
        fn prop_encrypt_wrong_key_fails(
            salt in prop::collection::vec(any::<u8>(), 16),
            verifier in prop::collection::vec(any::<u8>(), 32),
            key_a in prop::collection::vec(any::<u8>(), 32),
            key_b in prop::collection::vec(any::<u8>(), 32),
        ) {
            prop_assume!(key_a != key_b);

            let (enc_salt, enc_verifier) = encrypt_phrase_data(&salt, &verifier, &key_a).unwrap();

            // Decrypting with a different key should fail
            let result = decrypt_phrase_data(&enc_salt, &enc_verifier, &key_b);
            prop_assert!(result.is_err(), "decrypting with a different key must fail");
        }
    }
}
