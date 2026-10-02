//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence value arithmetic shared by every provider, following `PostgreSQL`'s `nextval`.

/// Values a sequence record covers ahead of the ones handed out, as `PostgreSQL`'s `SEQ_LOG_VALS`.
const LOGGED_AHEAD: i128 = 32;

/// Physical sequence position consumed by one atomic reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceValuePosition {
    pub current: i64,
    pub called: bool,
    pub log_count: i64,
}

/// One atomic sequence reservation. `first_value` is returned immediately, while the remaining values through `last_value` belong to the allocating session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceValueReservation {
    pub first_value: i64,
    pub last_value: i64,
    pub count: i64,
    pub log_count: i64,
}

/// A reservation with the durable position it requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceValueAllocation {
    pub reservation: SequenceValueReservation,
    /// The value the durable record must hold, as called and with no log count, before any value of the reservation is handed out. It is set when the reservation uses up the values an earlier record covered, and lies up to 32 fetches past the reservation, where `PostgreSQL` writes it to its log. A sequence that loses its exact position continues after this value, so no value at or below it is handed out twice.
    pub logged_value: Option<i64>,
}

impl SequenceValueAllocation {
    /// The value the durable record must hold, as called and with no log count, before the reservation is handed out, or `None` when the record covers the reservation already. `position` is the position the reservation continues. `covering` is the value of the record known to cover that position, or `None` when the position was read from a record, which covers nothing past itself.
    #[must_use]
    pub fn record_value(
        &self,
        position: SequenceValuePosition,
        covering: Option<i64>,
        increment: i64,
        min_value: i64,
        max_value: i64,
    ) -> Option<i64> {
        if self.logged_value.is_some() {
            return self.logged_value;
        }
        let reservation = self.reservation;
        let ascending = increment > 0;
        // A reservation that starts over from the opposite bound leaves every earlier record behind it.
        let wrapped = position.called
            && if ascending {
                reservation.first_value <= position.current
            } else {
                reservation.first_value >= position.current
            };
        let covered = covering.is_some_and(|covering| {
            !wrapped
                && if ascending {
                    reservation.last_value <= covering
                } else {
                    reservation.last_value >= covering
                }
        });
        if covered {
            return None;
        }
        // Each reserved value uses up one of the log count, so the count that remains reaches the value `PostgreSQL`'s log holds for this position.
        let ahead = i128::from(reservation.last_value)
            + i128::from(increment) * i128::from(reservation.log_count);
        let bounded = ahead.clamp(i128::from(min_value), i128::from(max_value));
        Some(i64::try_from(bounded).expect("a value within the sequence bounds"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceReservationResult {
    Missing,
    DefinitionChanged,
    Exhausted,
    Reserved(SequenceValueReservation),
}

/// A value update applies only to the definition whose bounds the caller validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceSetValueResult {
    Missing,
    DefinitionChanged,
    Set(i64),
}

/// The outcome of moving a sequence's durable position from an expected one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceLogResult {
    Missing,
    DefinitionChanged,
    /// The record holds this position instead of the expected one; nothing was written.
    Changed(SequenceValuePosition),
    Logged,
}

/// Reserve up to `cache_size` values without crossing a sequence bound. Cycling is applied when selecting the first value of a new reservation, matching `PostgreSQL`'s boundary-truncated cache blocks.
#[must_use]
pub fn sequence_value_reservation(
    position: SequenceValuePosition,
    increment: i64,
    min_value: i64,
    max_value: i64,
    cycle: bool,
    cache_size: i64,
) -> Option<SequenceValueReservation> {
    sequence_value_allocation(position, increment, min_value, max_value, cycle, cache_size)
        .map(|allocation| allocation.reservation)
}

/// [`sequence_value_reservation`] with the durable position the reservation requires.
#[must_use]
pub fn sequence_value_allocation(
    position: SequenceValuePosition,
    increment: i64,
    min_value: i64,
    max_value: i64,
    cycle: bool,
    cache_size: i64,
) -> Option<SequenceValueAllocation> {
    let SequenceValuePosition {
        current,
        called,
        log_count,
    } = position;
    debug_assert_ne!(increment, 0);
    debug_assert!(cache_size > 0);
    let first_value = if called {
        match current
            .checked_add(increment)
            .filter(|value| (min_value..=max_value).contains(value))
        {
            Some(value) => value,
            None if cycle && increment > 0 => min_value,
            None if cycle => max_value,
            None => return None,
        }
    } else {
        current
    };
    let distance = if increment > 0 {
        i128::from(max_value) - i128::from(first_value)
    } else {
        i128::from(first_value) - i128::from(min_value)
    };
    let step = i128::from(increment).abs();
    let available = distance / step + 1;
    let count = available.min(i128::from(cache_size));
    let last_value = i128::from(first_value) + i128::from(increment) * (count - 1);
    let initial_count = i128::from(!called);
    let cache_fetch = i128::from(cache_size) - initial_count;
    let mut fetch = cache_fetch;
    let mut next_log_count = i128::from(log_count);
    let logged = i128::from(log_count) < cache_fetch || !called;
    if logged {
        fetch += LOGGED_AHEAD;
        next_log_count = fetch;
    }
    let fetched = fetch.min(available - initial_count);
    next_log_count -= fetched.min(cache_fetch);
    next_log_count -= fetch - fetched;
    // The fetches advance from the first value, which an uncalled sequence returns without advancing.
    let logged_value = logged.then(|| {
        let ahead = i128::from(first_value) + i128::from(increment) * (fetched - 1 + initial_count);
        i64::try_from(ahead).expect("logged sequence value stays in bounds")
    });
    Some(SequenceValueAllocation {
        reservation: SequenceValueReservation {
            first_value,
            last_value: i64::try_from(last_value).expect("reserved sequence value stays in bounds"),
            count: i64::try_from(count).expect("reservation count cannot exceed cache size"),
            log_count: i64::try_from(next_log_count)
                .expect("persisted sequence log count cannot exceed the cache request"),
        },
        logged_value,
    })
}

#[cfg(test)]
mod tests;
