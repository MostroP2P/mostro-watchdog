//! One-time codes that tie a solver key to the Telegram chat that sent
//! `/link` (SOLVER_NOTIFICATIONS.md, `link`).

use std::time::Duration;

use nostr_sdk::prelude::SecretKey;

/// Letters and digits a code is made of: no `0`/`O`, `1`/`I`/`L`, so a code
/// read off a phone screen is typed right.
pub const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// Characters in a code, shown as two groups of four.
const CODE_LEN: usize = 8;
const GROUP_LEN: usize = 4;

/// How long a code works after `/link`.
pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);

/// Largest multiple of the alphabet's size that fits in a byte: bytes at or
/// above it are skipped, so every character is equally likely.
const UNBIASED_LIMIT: u8 = (256 / CODE_ALPHABET.len() * CODE_ALPHABET.len()) as u8;

/// A new random code, e.g. `K7QM-2XPA`.
pub fn generate() -> String {
    let mut chars = Vec::with_capacity(CODE_LEN);
    while chars.len() < CODE_LEN {
        // A fresh secret key is 32 bytes from the OS random source.
        let bytes = SecretKey::generate().to_secret_bytes();
        chars.extend(
            bytes
                .iter()
                .filter(|&&b| b < UNBIASED_LIMIT)
                .map(|&b| CODE_ALPHABET[usize::from(b) % CODE_ALPHABET.len()])
                .take(CODE_LEN - chars.len()),
        );
    }
    format(&chars)
}

/// `input` as a code in its canonical form (`K7QM-2XPA`), when it is one:
/// any case, with or without the dash and surrounding spaces.
pub fn normalize(input: &str) -> Option<String> {
    let chars: Vec<u8> = input
        .trim()
        .bytes()
        .filter(|&b| b != b'-')
        .map(|b| b.to_ascii_uppercase())
        .collect();
    let valid = chars.len() == CODE_LEN && chars.iter().all(|b| CODE_ALPHABET.contains(b));
    valid.then(|| format(&chars))
}

fn format(chars: &[u8]) -> String {
    let (head, tail) = chars.split_at(GROUP_LEN);
    format!(
        "{}-{}",
        String::from_utf8_lossy(head),
        String::from_utf8_lossy(tail)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_codes_are_canonical() {
        for _ in 0..100 {
            let code = generate();

            assert_eq!(code.len(), CODE_LEN + 1, "{code}");
            assert_eq!(normalize(&code).as_deref(), Some(code.as_str()));
        }
    }

    #[test]
    fn generated_codes_differ() {
        let codes: HashSet<String> = (0..50).map(|_| generate()).collect();

        assert_eq!(codes.len(), 50);
    }

    #[test]
    fn codes_are_accepted_in_any_case_with_or_without_the_dash() {
        for input in ["K7QM-2XPA", "k7qm-2xpa", "K7QM2XPA", "  k7qm2xpa \n"] {
            assert_eq!(normalize(input).as_deref(), Some("K7QM-2XPA"), "{input:?}");
        }
    }

    #[test]
    fn anything_else_is_not_a_code() {
        // Too short, too long, ambiguous characters, not ASCII.
        for input in [
            "",
            "K7QM-2XP",
            "K7QM-2XPAA",
            "K7QM-2XP0",
            "K7QM-2XPI",
            "K7QM-2XPÁ",
        ] {
            assert_eq!(normalize(input), None, "{input:?}");
        }
    }

    #[test]
    fn every_byte_below_the_limit_maps_evenly() {
        assert_eq!(usize::from(UNBIASED_LIMIT) % CODE_ALPHABET.len(), 0);
    }
}
