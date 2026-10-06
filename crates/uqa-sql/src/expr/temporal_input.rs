//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal input context for analysis and worker execution. Guards restore the caller's clock and date order on every exit.

use std::{cell::Cell, marker::PhantomData};
use uqa_core::TemporalDateOrder;

thread_local! {
    static DATE_ORDER: Cell<TemporalDateOrder> = const { Cell::new(TemporalDateOrder::MonthDayYear) };
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

/// A copyable context passed from a statement to its workers.
#[derive(Clone, Copy)]
pub struct TemporalInputContext {
    pub transaction_clock_micros: Option<i64>,
    pub date_order: TemporalDateOrder,
}

impl TemporalInputContext {
    #[must_use]
    pub fn current() -> Self {
        Self {
            transaction_clock_micros: super::transaction_clock_micros(),
            date_order: temporal_date_order(),
        }
    }

    #[must_use]
    pub fn enter(self) -> TemporalInputScope {
        TemporalInputScope {
            _clock: self
                .transaction_clock_micros
                .map(super::TransactionClockScope::enter),
            _order: DateOrderScope::enter(self.date_order),
        }
    }
}

pub struct TemporalInputScope {
    _clock: Option<super::TransactionClockScope>,
    _order: DateOrderScope,
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
        }
        .enter();
        {
            let _nested = TemporalInputContext {
                transaction_clock_micros: Some(20),
                date_order: TemporalDateOrder::YearMonthDay,
            }
            .enter();
            assert_eq!(super::super::transaction_clock_micros(), Some(20));
            assert!(crate::expr::cast_value(&Value::Str("13/02/2020".into()), "date").is_err());
        }
        assert_eq!(super::super::transaction_clock_micros(), Some(10));
        assert_eq!(temporal_date_order(), TemporalDateOrder::DayMonthYear);
        let Value::Temporal(value) =
            crate::expr::cast_value(&Value::Str("02/03/2020".into()), "date").unwrap()
        else {
            panic!("date")
        };
        assert_eq!(value.to_sql_string(), "2020-03-02");
    }
}
