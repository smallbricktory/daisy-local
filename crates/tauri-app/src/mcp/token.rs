//! Bearer token for the loopback MCP server: 32 random bytes, base64url.
//! Stored in the encrypted vault (`DecryptedKeys.mcp_token`); exists in
//! memory only while the vault is unlocked.

use base64::Engine as _;
use rand::Rng as _;

/// Mints a fresh 256-bit token, base64url (no pad) → 43 chars.
pub fn generate() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Timing-safe comparison via SHA-256 digests.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    use sha2::{Digest, Sha256};
    Sha256::digest(expected.as_bytes()) == Sha256::digest(presented.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_is_long_and_unique() {
        let a = generate();
        let b = generate();
        assert!(a.len() >= 40, "32 bytes base64url = 43 chars");
        assert_ne!(a, b, "two mints must differ");
    }

    #[test]
    fn matches_only_exact() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", ""));
    }
}
