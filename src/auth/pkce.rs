//! RFC 7636 PKCE, plus the CSRF `state` nonce.
//!
//! Pure functions over a CSPRNG — no I/O, no clock. The split between
//! [`generate`] (random) and [`challenge_for`] (deterministic) is what
//! makes this testable: the challenge derivation is checked against
//! the RFC's own test vector.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A PKCE verifier/challenge pair.
///
/// The verifier is the secret; it never leaves the process until the
/// token exchange. The challenge is what goes in the authorize URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Number of random bytes behind a code verifier. 64 bytes
/// base64url-encodes to 86 chars, comfortably inside RFC 7636's
/// 43..=128 range.
const VERIFIER_BYTES: usize = 64;

/// Number of random bytes behind the `state` nonce.
const STATE_BYTES: usize = 32;

/// Generate a fresh verifier/challenge pair using the S256 method.
pub fn generate() -> Pkce {
    let verifier = random_b64url(VERIFIER_BYTES);
    let challenge = challenge_for(&verifier);
    Pkce {
        verifier,
        challenge,
    }
}

/// Derive the S256 challenge for `verifier`:
/// `BASE64URL-NOPAD(SHA256(ASCII(verifier)))`.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Generate an opaque CSRF `state` value for the authorize request.
pub fn generate_state() -> String {
    random_b64url(STATE_BYTES)
}

/// `n` CSPRNG bytes, base64url-encoded without padding.
fn random_b64url(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B.
    #[test]
    fn challenge_matches_rfc7636_test_vector() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn challenge_is_deterministic() {
        assert_eq!(challenge_for("abc"), challenge_for("abc"));
    }

    #[test]
    fn generated_verifier_is_within_rfc_length_bounds() {
        let p = generate();
        assert!(
            (43..=128).contains(&p.verifier.len()),
            "verifier length {} outside 43..=128",
            p.verifier.len()
        );
    }

    #[test]
    fn generated_verifier_uses_only_unreserved_characters() {
        // RFC 7636 restricts the verifier to `[A-Za-z0-9-._~]`.
        // base64url-nopad stays inside that set; padding (`=`) would
        // not, which is why the NO_PAD engine matters.
        let p = generate();
        assert!(
            p.verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')),
            "verifier contains reserved characters: {}",
            p.verifier
        );
    }

    #[test]
    fn generated_challenge_agrees_with_its_verifier() {
        let p = generate();
        assert_eq!(p.challenge, challenge_for(&p.verifier));
    }

    #[test]
    fn successive_generations_differ() {
        assert_ne!(generate().verifier, generate().verifier);
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn state_is_unpadded_base64url_of_32_bytes() {
        let s = generate_state();
        assert_eq!(s.len(), 43);
        assert!(!s.contains('='));
    }
}
