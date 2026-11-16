//! Random bytes from the operating system, and the one-time passwords an admin hands out.

/// `N` bytes from the OS's CSPRNG. There is no fallback: without the OS generator nothing
/// secret can be made, so this panics rather than return something guessable.
pub fn bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("the operating system's random number generator failed");
    buf
}

/// No `0/O`, `1/l/I`: the password is read off one screen and typed into another.
const TEMP_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
/// 20 symbols from 57: about 116 bits.
pub const TEMP_PASSWORD_LEN: usize = 20;

/// A one-time password for a new account or an admin reset. The person must change it at
/// their next sign-in.
pub fn temporary_password() -> String {
    // Rejection sampling keeps every symbol equally likely: 57 * 4 = 228, so bytes 228..=255
    // are thrown away.
    let limit = (256 / TEMP_ALPHABET.len() * TEMP_ALPHABET.len()) as u8;
    let mut out = String::with_capacity(TEMP_PASSWORD_LEN);
    while out.len() < TEMP_PASSWORD_LEN {
        for b in bytes::<32>() {
            if b < limit && out.len() < TEMP_PASSWORD_LEN {
                out.push(TEMP_ALPHABET[(b as usize) % TEMP_ALPHABET.len()] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_passwords_are_long_and_distinct() {
        let a = temporary_password();
        let b = temporary_password();
        assert_eq!(a.len(), TEMP_PASSWORD_LEN);
        assert_ne!(a, b);
        assert!(a.bytes().all(|c| TEMP_ALPHABET.contains(&c)));
    }

    #[test]
    fn random_bytes_differ() {
        assert_ne!(bytes::<32>(), bytes::<32>());
    }
}
