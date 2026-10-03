// ABOUTME: Unguessable identifiers minted from the OS CSPRNG, rendered as lowercase hex
// ABOUTME: The one source of task ids, session ids and server-to-client request ids
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Identifiers that act as bearer capabilities.
//!
//! A task id, a session id and the id of a request the server sends its
//! client all reach a caller the auth hook may not tell apart from another —
//! every anonymous caller of an unauthenticated server is the same caller —
//! so for them the id is the only thing between one client's state and
//! another's. Each is minted here, from the operating system's CSPRNG, never
//! from a counter or the clock.

/// Bytes of OS randomness behind a minted id: 128 bits, the same strength as
/// a random UUID, rendered as 32 lowercase hex characters.
const ID_BYTES: usize = 16;

/// Lowercase hexadecimal alphabet an id is rendered in.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Mint a fresh 128-bit identifier as 32 lowercase hex characters.
///
/// Hex keeps it visible ASCII, so it travels unchanged in a JSON string and
/// in an HTTP header (`Mcp-Session-Id` allows only visible ASCII).
///
/// # Errors
///
/// The operating system could not supply randomness.
pub(crate) fn random_hex_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; ID_BYTES];
    getrandom::fill(&mut bytes)?;
    let mut id = String::with_capacity(ID_BYTES * 2);
    for byte in bytes {
        id.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        id.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_32_hex_characters_and_differ() {
        let first = random_hex_id().expect("randomness"); // Safe: test assertion
        let second = random_hex_id().expect("randomness"); // Safe: test assertion
        assert_eq!(first.len(), 32);
        assert!(first
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(first, second);
    }
}
