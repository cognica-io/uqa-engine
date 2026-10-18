//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use uqa_core::{TemporalDateOrder, TemporalIntervalStyle};
use uqa_sql::expr::{
    check_statement_cancellation, statement_cancellation, temporal_date_order,
    temporal_interval_style, transaction_clock_micros,
};

/// Assert that the invoking thread runs under `engine`'s statement thread context with `clock`.
fn assert_session_context(engine: &Engine, clock: Option<i64>) {
    assert_eq!(transaction_clock_micros(), clock);
    assert_eq!(temporal_date_order(), TemporalDateOrder::DayMonthYear);
    assert_eq!(
        temporal_interval_style(),
        TemporalIntervalStyle::SqlStandard
    );
    assert!(statement_cancellation()
        .is_some_and(|token| token.shares_signal(&engine.cancellation_token())));
}

fn session_with_styles() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "SET DateStyle = 'ISO, DMY'; SET IntervalStyle = 'sql_standard'; CREATE TABLE direct_rows (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO direct_rows VALUES (1, 'stored')",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn the_statement_thread_context_reads_the_session_clock_styles_and_cancellation() {
    let engine = session_with_styles();
    let context = engine.statement_thread_context();
    assert_eq!(context.temporal_input.transaction_clock_micros, None);
    assert_eq!(
        context.temporal_input.date_order,
        TemporalDateOrder::DayMonthYear
    );
    assert_eq!(
        context.temporal_input.interval_style,
        TemporalIntervalStyle::SqlStandard
    );
    assert!(context
        .cancellation
        .is_some_and(|token| token.shares_signal(&engine.cancellation_token())));
    engine.begin().unwrap();
    engine.session.transactions.lock()[0].started_at_micros = 90_123_456_789;
    assert_eq!(
        engine
            .statement_thread_context()
            .temporal_input
            .transaction_clock_micros,
        Some(90_123_456_789)
    );
    engine.rollback().unwrap();
    assert_eq!(engine.session_transaction_clock_micros(), None);
}

#[test]
fn direct_reads_run_under_the_session_statement_context() {
    let engine = session_with_styles();
    assert!(statement_cancellation().is_none());
    engine
        .with_direct_read_snapshot(|engine| {
            let statement = engine.statement_timestamp_micros();
            assert_session_context(engine, Some(statement));
            Ok(())
        })
        .unwrap();
    assert!(statement_cancellation().is_none());
    assert_eq!(transaction_clock_micros(), None);
}

#[test]
fn evaluating_row_writes_run_under_the_session_statement_context() {
    let engine = session_with_styles();
    engine
        .with_evaluating_row_write_transaction(
            "direct_rows",
            1,
            uqa_sql::ast::LockStrength::ForUpdate,
            |engine| {
                // A memory engine opens no transaction for a direct write, so the session has no transaction clock.
                assert_session_context(engine, None);
                Ok(())
            },
        )
        .unwrap();
    engine.begin().unwrap();
    engine.session.transactions.lock()[0].started_at_micros = 90_123_456_789;
    engine
        .with_evaluating_row_write_transaction(
            "direct_rows",
            1,
            uqa_sql::ast::LockStrength::ForUpdate,
            |engine| {
                assert_session_context(engine, Some(90_123_456_789));
                engine.cancel();
                assert_eq!(
                    check_statement_cancellation(),
                    Err(uqa_core::QueryCancelled::USER_REQUEST)
                );
                engine.reset_cancellation();
                Ok(())
            },
        )
        .unwrap();
    engine.rollback().unwrap();
    assert!(statement_cancellation().is_none());
    assert_eq!(transaction_clock_micros(), None);
}
