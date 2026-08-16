//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::future::Future;

use crate::{SQLParam, SQLResult};

/// Common asynchronous SQL interface implemented by embedded and HTTP engines.
pub trait AsyncSQLEngine {
    type Error;

    fn sql<'a>(
        &'a self,
        query: &'a str,
        params: &'a [SQLParam],
    ) -> impl Future<Output = Result<SQLResult, Self::Error>> + Send + 'a;

    fn sql_batch<'a>(
        &'a self,
        statements: &'a [(&'a str, &'a [SQLParam])],
    ) -> impl Future<Output = Result<Vec<SQLResult>, Self::Error>> + Send + 'a;
}
