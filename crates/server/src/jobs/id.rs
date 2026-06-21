//! Non-guessable job identifiers.
//!
//! 128 bits of OS randomness (via `getrandom`) rendered as 32 lowercase hex
//! chars. That is far beyond brute-force / enumeration: an attacker cannot guess
//! another caller's job id, which (together with per-identity ownership scoping
//! in the handlers) prevents result leakage. We deliberately avoid pulling in
//! the `uuid` crate — a fixed-size random hex string is all the contract needs.

/// Generate a fresh, non-guessable job id (32 hex chars / 128 bits).
///
/// Falls back to a best-effort id only if the OS RNG fails, which on supported
/// platforms does not happen in practice; the fallback still mixes in the
/// thread id and an address to avoid a constant value.
pub fn generate_job_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Extremely unlikely. Avoid a fixed string: derive some entropy from a
        // stack address and the current thread's id hash so two fallbacks in
        // the same process still differ. This path is not relied upon for the
        // security property (we log if it ever triggers).
        tracing::error!("getrandom failed while generating a job id; using weak fallback");
        let stack_marker = &bytes as *const _ as usize;
        let tid = format!("{:?}", std::thread::current().id());
        let mut acc = stack_marker as u64 ^ 0x9E37_79B9_7F4A_7C15;
        for b in tid.as_bytes() {
            acc = acc.rotate_left(5) ^ (*b as u64);
        }
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = (acc >> ((i % 8) * 8)) as u8;
        }
    }
    let mut s = String::with_capacity(32);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_32_hex_chars() {
        let id = generate_job_id();
        assert_eq!(id.len(), 32);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn ids_are_unique_across_many() {
        let mut seen = HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(generate_job_id()), "duplicate job id generated");
        }
    }
}
