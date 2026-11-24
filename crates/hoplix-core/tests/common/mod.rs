use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

// Passwords

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn test_password() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let first = hasher.finish();
    hasher.write_u64(first);
    let second = hasher.finish();
    [first, second]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .map(|nibble| char::from(b'a' + nibble))
        .collect()
}
