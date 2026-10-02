//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reservations against `PostgreSQL`'s `nextval` loop.

use super::*;

const fn sequence_position(current: i64, called: bool, log_count: i64) -> SequenceValuePosition {
    SequenceValuePosition {
        current,
        called,
        log_count,
    }
}

/// `nextval_internal` of `PostgreSQL` 18 `sequence.c`, fetch by fetch: the value returned, the last value cached, how many were cached, the log count kept, and the value written to the log when a record is written.
fn postgresql_nextval(
    position: SequenceValuePosition,
    incby: i128,
    minv: i128,
    maxv: i128,
    cycle: bool,
    cache: i128,
) -> Option<(i128, i128, i128, i128, Option<i128>)> {
    let mut next = i128::from(position.current);
    let mut result = next;
    let mut last = next;
    let mut fetch = cache;
    let mut log = i128::from(position.log_count);
    let mut rescnt = 0;
    if !position.called {
        rescnt += 1;
        fetch -= 1;
    }
    let mut logit = false;
    if log < fetch || !position.called {
        fetch += 32;
        log = fetch;
        logit = true;
    }
    while fetch > 0 {
        if incby > 0 {
            if (maxv >= 0 && next > maxv - incby) || (maxv < 0 && next + incby > maxv) {
                if rescnt > 0 {
                    break;
                }
                if !cycle {
                    return None;
                }
                next = minv;
            } else {
                next += incby;
            }
        } else if (minv < 0 && next < minv - incby) || (minv >= 0 && next + incby < minv) {
            if rescnt > 0 {
                break;
            }
            if !cycle {
                return None;
            }
            next = maxv;
        } else {
            next += incby;
        }
        fetch -= 1;
        if rescnt < cache {
            log -= 1;
            rescnt += 1;
            last = next;
            if rescnt == 1 {
                result = next;
            }
        }
    }
    log -= fetch;
    assert!(log >= 0);
    Some((result, last, rescnt, log, logit.then_some(next)))
}

#[test]
fn sequence_reservations_track_postgresql_log_counts() {
    assert_eq!(
        sequence_value_reservation(sequence_position(1, false, 0), 1, 1, i64::MAX, false, 1),
        Some(SequenceValueReservation {
            first_value: 1,
            last_value: 1,
            count: 1,
            log_count: 32,
        })
    );
    assert_eq!(
        sequence_value_reservation(sequence_position(1, true, 32), 1, 1, i64::MAX, false, 1),
        Some(SequenceValueReservation {
            first_value: 2,
            last_value: 2,
            count: 1,
            log_count: 31,
        })
    );
    assert_eq!(
        sequence_value_reservation(sequence_position(1, false, 0), 1, 1, i64::MAX, false, 10),
        Some(SequenceValueReservation {
            first_value: 1,
            last_value: 10,
            count: 10,
            log_count: 32,
        })
    );
    assert_eq!(
        sequence_value_reservation(sequence_position(5, false, 0), 2, 3, 9, true, 3),
        Some(SequenceValueReservation {
            first_value: 5,
            last_value: 9,
            count: 3,
            log_count: 0,
        })
    );
    assert_eq!(
        sequence_value_reservation(
            sequence_position(1, false, 0),
            1,
            1,
            i64::MAX,
            false,
            i64::MAX,
        ),
        Some(SequenceValueReservation {
            first_value: 1,
            last_value: i64::MAX,
            count: i64::MAX,
            log_count: 0,
        })
    );
}

#[test]
fn a_reservation_names_the_value_postgresql_logs() {
    let allocate = |position, cache| {
        sequence_value_allocation(position, 1, 1, i64::MAX, false, cache)
            .unwrap()
            .logged_value
    };
    // The first call logs 32 fetches past the value it returns, and the next 32 calls log nothing.
    assert_eq!(allocate(sequence_position(1, false, 0), 1), Some(33));
    assert_eq!(allocate(sequence_position(1, true, 32), 1), None);
    assert_eq!(allocate(sequence_position(32, true, 1), 1), None);
    assert_eq!(allocate(sequence_position(33, true, 0), 1), Some(66));
    // A cached block is logged 32 fetches past its last value.
    assert_eq!(allocate(sequence_position(1, false, 0), 10), Some(42));
    assert_eq!(allocate(sequence_position(10, true, 32), 10), None);
    assert_eq!(allocate(sequence_position(40, true, 2), 10), Some(82));
    // A bound stops the fetches, and the record then holds the bound.
    assert_eq!(
        sequence_value_allocation(sequence_position(5, false, 0), 2, 3, 9, true, 3)
            .unwrap()
            .logged_value,
        Some(9)
    );
    assert_eq!(
        sequence_value_allocation(sequence_position(9, true, 0), 2, 3, 9, true, 1)
            .unwrap()
            .logged_value,
        Some(9)
    );
}

#[test]
fn every_small_reservation_equals_postgresql_fetch_by_fetch() {
    let mut compared = 0_u32;
    for (increment, min_value, max_value) in [
        (1, 1, 50),
        (2, 3, 41),
        (3, -20, 20),
        (7, 0, 100),
        (-1, -50, -1),
        (-2, -41, -3),
        (-3, -20, 20),
        (-7, 0, 100),
        (1, -3, 3),
        (-1, -3, 3),
    ] {
        for cycle in [false, true] {
            for cache_size in [1, 2, 3, 5, 40] {
                for current in min_value..=max_value {
                    for called in [false, true] {
                        for log_count in [0, 1, 2, 4, 5, 31, 32, 33, 39, 40, 72] {
                            let position = sequence_position(current, called, log_count);
                            let expected = postgresql_nextval(
                                position,
                                i128::from(increment),
                                i128::from(min_value),
                                i128::from(max_value),
                                cycle,
                                i128::from(cache_size),
                            );
                            let actual = sequence_value_allocation(
                                position, increment, min_value, max_value, cycle, cache_size,
                            )
                            .map(|allocation| {
                                (
                                    i128::from(allocation.reservation.first_value),
                                    i128::from(allocation.reservation.last_value),
                                    i128::from(allocation.reservation.count),
                                    i128::from(allocation.reservation.log_count),
                                    allocation.logged_value.map(i128::from),
                                )
                            });
                            assert_eq!(
                                actual, expected,
                                "{position:?} increment {increment} bounds {min_value}..={max_value} cycle {cycle} cache {cache_size}"
                            );
                            compared += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(compared > 100_000);
}

#[test]
fn a_record_is_required_when_postgresql_logs_or_nothing_covers_the_reservation() {
    let record = |position, covering| {
        sequence_value_allocation(position, 1, 1, i64::MAX, false, 1)
            .unwrap()
            .record_value(position, covering, 1, 1, i64::MAX)
    };
    // The value `PostgreSQL` logs is the record, whatever covered the position before.
    assert_eq!(record(sequence_position(1, false, 0), None), Some(33));
    assert_eq!(record(sequence_position(33, true, 0), Some(33)), Some(66));
    // Between two logs the record written by the first covers every reservation.
    assert_eq!(record(sequence_position(1, true, 32), Some(33)), None);
    assert_eq!(record(sequence_position(32, true, 1), Some(33)), None);
    // A position read from a record is covered by nothing, so the record moves to where the log count reaches.
    assert_eq!(record(sequence_position(1, true, 32), None), Some(33));
    assert_eq!(record(sequence_position(7, true, 5), None), Some(12));
    // A record that lies before the reservation does not cover it.
    assert_eq!(record(sequence_position(7, true, 5), Some(7)), Some(12));
    // A log count that reaches past the bound is cut at the bound.
    let position = sequence_position(7, true, 5);
    assert_eq!(
        sequence_value_allocation(position, 1, 1, 10, false, 1)
            .unwrap()
            .record_value(position, None, 1, 1, 10),
        Some(10)
    );
    // A reservation that starts over at the other bound is past every earlier record.
    let position = sequence_position(9, true, 0);
    let wrapped = sequence_value_allocation(position, 2, 3, 9, true, 1).unwrap();
    assert_eq!(wrapped.reservation.first_value, 3);
    assert_eq!(wrapped.record_value(position, Some(9), 2, 3, 9), Some(9));
}

/// A sequence keeps its exact position outside its record and may lose it at any time. It then continues from the record, so the record has to stay at or past every value handed out.
#[test]
fn a_sequence_that_loses_its_position_never_repeats_a_value() {
    let mut reservations = 0_u32;
    for (increment, min_value, max_value) in [
        (1_i64, 1_i64, 4_000_i64),
        (3, -50, 9_000),
        (-1, -4_000, -1),
        (-7, -30_000, 12),
    ] {
        let ascending = increment > 0;
        for cache_size in [1, 2, 5, 40] {
            for log_count in [0, 1, 5, 32, 40] {
                for called in [false, true] {
                    for lost_every in [1, 2, 7, 33, 1_000] {
                        let start = if ascending { min_value } else { max_value };
                        // An exact record, as an allocation that keeps no volatile position writes it.
                        let mut record = sequence_position(start, called, log_count);
                        let mut position: Option<(i64, SequenceValuePosition)> = None;
                        let mut last_handed_out = called.then_some(start);
                        for step in 1..=120 {
                            if step % lost_every == 0 {
                                position = None;
                            }
                            let (base, covering) = match position {
                                Some((covering, exact)) => (exact, Some(covering)),
                                None => (record, None),
                            };
                            let Some(allocation) = sequence_value_allocation(
                                base, increment, min_value, max_value, false, cache_size,
                            ) else {
                                break;
                            };
                            if let Some(value) = allocation
                                .record_value(base, covering, increment, min_value, max_value)
                            {
                                record = sequence_position(value, true, 0);
                            }
                            let reservation = allocation.reservation;
                            if let Some(previous) = last_handed_out {
                                assert!(
                                    if ascending {
                                        reservation.first_value > previous
                                    } else {
                                        reservation.first_value < previous
                                    },
                                    "{reservation:?} repeats a value at or before {previous}"
                                );
                            }
                            assert!(record.called);
                            assert!(
                                if ascending {
                                    reservation.last_value <= record.current
                                } else {
                                    reservation.last_value >= record.current
                                },
                                "{record:?} does not cover {reservation:?}"
                            );
                            last_handed_out = Some(reservation.last_value);
                            position = Some((
                                record.current,
                                sequence_position(
                                    reservation.last_value,
                                    true,
                                    reservation.log_count,
                                ),
                            ));
                            reservations += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(reservations > 50_000);
}

/// While the position is kept, the values and log counts are the ones `PostgreSQL` produces, and the record is the value its log holds.
#[test]
fn a_kept_position_follows_postgresql_and_its_log() {
    for (increment, min_value, max_value, cycle) in [
        (1_i64, 1_i64, i64::MAX, false),
        (2, 3, 41, true),
        (-3, -20, 20, true),
        (-1, i64::MIN, -1, false),
    ] {
        for cache_size in [1, 3, 40] {
            let start = if increment > 0 { min_value } else { max_value };
            let mut record = sequence_position(start, false, 0);
            let mut covering = None;
            let mut exact = record;
            let mut postgresql = record;
            let mut postgresql_log = None;
            for _ in 0..300 {
                let allocation = sequence_value_allocation(
                    exact, increment, min_value, max_value, cycle, cache_size,
                )
                .unwrap();
                if let Some(value) =
                    allocation.record_value(exact, covering, increment, min_value, max_value)
                {
                    record = sequence_position(value, true, 0);
                }
                let (first, last, count, log, logged) = postgresql_nextval(
                    postgresql,
                    i128::from(increment),
                    i128::from(min_value),
                    i128::from(max_value),
                    cycle,
                    i128::from(cache_size),
                )
                .unwrap();
                if logged.is_some() {
                    postgresql_log = logged;
                }
                let reservation = allocation.reservation;
                assert_eq!(
                    (
                        i128::from(reservation.first_value),
                        i128::from(reservation.last_value),
                        i128::from(reservation.count),
                        i128::from(reservation.log_count),
                    ),
                    (first, last, count, log)
                );
                assert_eq!(Some(i128::from(record.current)), postgresql_log);
                exact = sequence_position(reservation.last_value, true, reservation.log_count);
                postgresql = exact;
                covering = Some(record.current);
            }
        }
    }
}
