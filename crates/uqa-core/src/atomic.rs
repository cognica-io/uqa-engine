//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Conditional atomic updates using the compare/exchange interface supported by the MSRV.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

macro_rules! updater {
    ($name:ident, $atomic:ty, $value:ty) => {
        /// Return the previous value after a successful update, or the observed value when the callback returns `None`. The callback can run repeatedly after contention; the orderings apply to the successful exchange and failed reads respectively.
        pub fn $name(
            atomic: &$atomic,
            set_order: Ordering,
            fetch_order: Ordering,
            mut update: impl FnMut($value) -> Option<$value>,
        ) -> Result<$value, $value> {
            let mut previous = atomic.load(fetch_order);
            loop {
                let next = update(previous).ok_or(previous)?;
                match atomic.compare_exchange_weak(previous, next, set_order, fetch_order) {
                    Ok(value) => return Ok(value),
                    Err(value) => previous = value,
                }
            }
        }
    };
}

updater!(try_update_u64, AtomicU64, u64);
updater!(try_update_i32, AtomicI32, i32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_unsigned_and_signed_identities_do_not_wrap() {
        let unsigned = AtomicU64::new(u64::MAX);
        assert_eq!(
            try_update_u64(&unsigned, Ordering::Relaxed, Ordering::Relaxed, |id| id
                .checked_add(1)),
            Err(u64::MAX)
        );
        assert_eq!(unsigned.load(Ordering::Relaxed), u64::MAX);
        let signed = AtomicI32::new(i32::MAX);
        assert_eq!(
            try_update_i32(&signed, Ordering::Relaxed, Ordering::Relaxed, |id| id
                .checked_add(1)),
            Err(i32::MAX)
        );
        assert_eq!(signed.load(Ordering::Relaxed), i32::MAX);
        let zero = AtomicU64::new(0);
        assert_eq!(
            try_update_u64(&zero, Ordering::AcqRel, Ordering::Acquire, |n| n
                .checked_sub(1)),
            Err(0)
        );
        assert_eq!(zero.load(Ordering::Acquire), 0);
    }

    #[test]
    fn changed_value_is_reloaded_before_retrying_the_callback() {
        let atomic = AtomicU64::new(1);
        let mut first = true;
        let previous = try_update_u64(&atomic, Ordering::AcqRel, Ordering::Acquire, |value| {
            if first {
                first = false;
                atomic.store(9, Ordering::Release);
            }
            value.checked_add(1)
        });
        assert_eq!(previous, Ok(9));
        assert_eq!(atomic.load(Ordering::Acquire), 10);
    }

    #[test]
    fn concurrent_updates_preserve_every_increment() {
        let atomic = AtomicI32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..256 {
                        try_update_i32(&atomic, Ordering::Relaxed, Ordering::Relaxed, |n| {
                            n.checked_add(1)
                        })
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(atomic.load(Ordering::Relaxed), 1024);
    }
}
