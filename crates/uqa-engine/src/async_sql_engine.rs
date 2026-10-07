//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::future::{poll_fn, Future};
use std::task::Poll;

use uqa_sql::{AsyncSQLEngine, SQLError, SQLParam, SQLResult};

use crate::Engine;

impl AsyncSQLEngine for Engine {
    type Error = SQLError;

    fn sql<'a>(
        &'a self,
        query: &'a str,
        params: &'a [SQLParam],
    ) -> impl Future<Output = Result<SQLResult, Self::Error>> + Send + 'a {
        poll_fn(move |_| Poll::Ready(Engine::sql(self, query, params)))
    }

    fn sql_batch<'a>(
        &'a self,
        statements: &'a [(&'a str, &'a [SQLParam])],
    ) -> impl Future<Output = Result<Vec<SQLResult>, Self::Error>> + Send + 'a {
        poll_fn(move |_| Poll::Ready(Engine::sql_batch(self, statements)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Waker};

    #[test]
    fn a_sql_future_executes_only_when_polled() {
        let engine = Engine::new();
        let query = "CREATE TABLE async_single (id INTEGER)";
        drop(AsyncSQLEngine::sql(&engine, query, &[]));
        assert!(!engine.has_table("async_single").unwrap());
        let mut future = std::pin::pin!(AsyncSQLEngine::sql(&engine, query, &[]));
        assert!(!engine.has_table("async_single").unwrap());
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            future.as_mut().poll(&mut context),
            Poll::Ready(Ok(_))
        ));
        assert!(engine.has_table("async_single").unwrap());
    }

    #[test]
    fn a_batch_future_executes_only_when_polled() {
        let engine = Engine::new();
        let statements: &[(&str, &[SQLParam])] = &[
            ("CREATE TABLE async_batch (id INTEGER)", &[]),
            ("INSERT INTO async_batch VALUES (1)", &[]),
        ];
        drop(AsyncSQLEngine::sql_batch(&engine, statements));
        assert!(!engine.has_table("async_batch").unwrap());
        let mut future = std::pin::pin!(AsyncSQLEngine::sql_batch(&engine, statements));
        assert!(!engine.has_table("async_batch").unwrap());
        let mut context = Context::from_waker(Waker::noop());
        let Poll::Ready(Ok(results)) = future.as_mut().poll(&mut context) else {
            panic!("embedded SQL batch must complete on its first poll");
        };
        assert_eq!(results.len(), 2);
        assert_eq!(
            engine
                .sql("SELECT id FROM async_batch", &[])
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}
