//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_sql::{AsyncSQLEngine, SQLError, SQLParam, SQLResult};

use crate::Engine;

impl AsyncSQLEngine for Engine {
    type Error = SQLError;

    async fn sql<'a>(
        &'a self,
        query: &'a str,
        params: &'a [SQLParam],
    ) -> Result<SQLResult, Self::Error> {
        Engine::sql(self, query, params)
    }

    async fn sql_batch<'a>(
        &'a self,
        statements: &'a [(&'a str, &'a [SQLParam])],
    ) -> Result<Vec<SQLResult>, Self::Error> {
        Engine::sql_batch(self, statements)
    }
}
