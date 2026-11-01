//! Canonical-body hashing.
//!
//! Contract: docs/implementation/06 §Hashing policy. The caller hashes the
//! body WITHOUT the contextual header — stripping the header is the ingest
//! layer's job (docs/implementation/07); these helpers normalize line
//! endings to LF and hash UTF-8 bytes. blake3 is the in-memory prefilter
//! and never leaves the process.

use sha2::Sha256;

/// Normalizes a chunk body to the canonical form: UTF-8 bytes with LF line
/// endings.
#[must_use]
pub fn canonical_body(body: &str) -> std::borrow::Cow<'_, str> {
    if body.contains('\r') {
        body.replace("\r\n", "\n").replace('\r', "\n").into()
    } else {
        body.into()
    }
}

/// SHA-256 of the canonical body — the value stored in
/// `chunks.content_hash`.
///
/// `body` is the chunk body without the contextual header; the header is
/// prepended later, when the stored `text` is assembled.
#[must_use]
pub fn content_hash(body: &str) -> [u8; 32] {
    use sha2::Digest;
    let canonical = canonical_body(body);
    Sha256::digest(canonical.as_bytes()).into()
}

/// blake3 of the canonical body, the in-memory prefilter hash for the ingest
/// hot path.
///
/// blake3 hashes never leave the process and are never persisted; exact
/// dedup always confirms with [`content_hash`].
#[must_use]
pub fn fast_hash(body: &str) -> blake3::Hash {
    blake3::hash(canonical_body(body).as_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{canonical_body, content_hash, fast_hash};

    const ABC_SHA256: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];

    /// SHA-256 over the canonical form of "abc" matches the standard test
    /// vector.
    #[test]
    fn content_hash_matches_the_sha256_vector() {
        assert_eq!(content_hash("abc"), ABC_SHA256);
    }

    /// CRLF and bare CR normalize to LF before hashing.
    #[test]
    fn content_hash_normalizes_line_endings() {
        assert_eq!(content_hash("a\r\nb\rc\n"), content_hash("a\nb\nc\n"));
    }

    /// Canonicalization is a no-op copy when nothing has to change.
    #[test]
    fn canonical_body_is_borrowed_when_clean() {
        let body = "no carriage returns here";
        assert!(matches!(
            canonical_body(body),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// blake3 over the canonical form matches the standard test vector, and
    /// line-ending normalization feeds it the same bytes.
    #[test]
    fn fast_hash_matches_the_blake3_vector() {
        let expected: [u8; 32] = *blake3::hash(b"abc").as_bytes();
        assert_eq!(fast_hash("abc"), blake3::Hash::from(expected));
        assert_eq!(fast_hash("a\r\nb"), fast_hash("a\nb"));
    }
}
