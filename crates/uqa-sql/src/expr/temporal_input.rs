//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal context for analysis and worker execution. Guards restore the caller's clock, date order and interval input/output style on every exit.

use std::{cell::Cell, marker::PhantomData};
use uqa_core::{TemporalDateOrder, TemporalIntervalStyle};

thread_local! {
    static DATE_ORDER: Cell<TemporalDateOrder> = const { Cell::new(TemporalDateOrder::MonthDayYear) };
    static INTERVAL_STYLE: Cell<TemporalIntervalStyle> = const { Cell::new(TemporalIntervalStyle::Postgres) };
}

#[must_use]
pub fn temporal_interval_style() -> TemporalIntervalStyle {
    INTERVAL_STYLE.with(Cell::get)
}

/// Restore the invoking thread's interval input/output style on every exit.
pub struct IntervalStyleScope {
    previous: TemporalIntervalStyle,
    thread: PhantomData<std::rc::Rc<()>>,
}

impl IntervalStyleScope {
    #[must_use]
    pub fn enter(style: TemporalIntervalStyle) -> Self {
        Self {
            previous: INTERVAL_STYLE.with(|current| current.replace(style)),
            thread: PhantomData,
        }
    }
}

impl Drop for IntervalStyleScope {
    fn drop(&mut self) {
        INTERVAL_STYLE.with(|current| current.set(self.previous));
    }
}

#[must_use]
pub fn temporal_date_order() -> TemporalDateOrder {
    DATE_ORDER.with(Cell::get)
}

/// Restore the calling thread's date order when the input scope ends.
pub struct DateOrderScope {
    previous: TemporalDateOrder,
    thread: PhantomData<std::rc::Rc<()>>,
}

impl DateOrderScope {
    #[must_use]
    pub fn enter(order: TemporalDateOrder) -> Self {
        Self {
            previous: DATE_ORDER.with(|current| current.replace(order)),
            thread: PhantomData,
        }
    }
}

impl Drop for DateOrderScope {
    fn drop(&mut self) {
        DATE_ORDER.with(|current| current.set(self.previous));
    }
}

/// A copyable context passed from a statement to its workers. Entering it reproduces every captured input exactly, including the absence of a transaction clock, so a thread never keeps an earlier statement's clock.
#[derive(Clone, Copy)]
pub struct TemporalInputContext {
    pub transaction_clock_micros: Option<i64>,
    pub date_order: TemporalDateOrder,
    pub interval_style: TemporalIntervalStyle,
}

impl TemporalInputContext {
    #[must_use]
    pub fn current() -> Self {
        Self {
            transaction_clock_micros: super::transaction_clock_micros(),
            date_order: temporal_date_order(),
            interval_style: temporal_interval_style(),
        }
    }

    #[must_use]
    pub fn enter(self) -> TemporalInputScope {
        TemporalInputScope {
            _clock: super::TransactionClockScope::enter(self.transaction_clock_micros),
            _order: DateOrderScope::enter(self.date_order),
            _interval: IntervalStyleScope::enter(self.interval_style),
        }
    }
}

pub struct TemporalInputScope {
    _clock: super::TransactionClockScope,
    _order: DateOrderScope,
    _interval: IntervalStyleScope,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;

    #[test]
    fn nested_input_contexts_restore_clocks_and_orders_after_failure() {
        let _caller = TemporalInputContext {
            transaction_clock_micros: Some(10),
            date_order: TemporalDateOrder::DayMonthYear,
            interval_style: TemporalIntervalStyle::SqlStandard,
        }
        .enter();
        {
            let _nested = TemporalInputContext {
                transaction_clock_micros: Some(20),
                date_order: TemporalDateOrder::YearMonthDay,
                interval_style: TemporalIntervalStyle::Iso8601,
            }
            .enter();
            assert_eq!(super::super::transaction_clock_micros(), Some(20));
            assert_eq!(temporal_interval_style(), TemporalIntervalStyle::Iso8601);
            assert!(crate::expr::cast_value(&Value::Str("13/02/2020".into()), "date").is_err());
        }
        assert_eq!(super::super::transaction_clock_micros(), Some(10));
        assert_eq!(temporal_date_order(), TemporalDateOrder::DayMonthYear);
        assert_eq!(
            temporal_interval_style(),
            TemporalIntervalStyle::SqlStandard
        );
        let Value::Temporal(value) =
            crate::expr::cast_value(&Value::Str("02/03/2020".into()), "date").unwrap()
        else {
            panic!("date")
        };
        assert_eq!(value.to_sql_string(), "2020-03-02");
    }

    #[test]
    fn a_context_without_a_clock_clears_the_thread_clock_until_it_ends() {
        let _caller = TemporalInputContext {
            transaction_clock_micros: Some(10),
            date_order: TemporalDateOrder::DayMonthYear,
            interval_style: TemporalIntervalStyle::SqlStandard,
        }
        .enter();
        let captured = TemporalInputContext::current();
        assert_eq!(captured.transaction_clock_micros, Some(10));
        {
            let _detached = TemporalInputContext {
                transaction_clock_micros: None,
                ..captured
            }
            .enter();
            assert_eq!(super::super::transaction_clock_micros(), None);
            assert!(super::super::transaction_timestamp_or_clock() > 1_700_000_000_000_000);
            assert_eq!(
                TemporalInputContext::current().transaction_clock_micros,
                None
            );
            {
                let _restored = captured.enter();
                assert_eq!(super::super::transaction_clock_micros(), Some(10));
            }
            assert_eq!(super::super::transaction_clock_micros(), None);
        }
        assert_eq!(super::super::transaction_clock_micros(), Some(10));
        std::thread::spawn(move || {
            let _worker = TemporalInputContext {
                transaction_clock_micros: None,
                ..captured
            }
            .enter();
            assert_eq!(super::super::transaction_clock_micros(), None);
            assert_eq!(temporal_date_order(), TemporalDateOrder::DayMonthYear);
        })
        .join()
        .unwrap();
    }
}
