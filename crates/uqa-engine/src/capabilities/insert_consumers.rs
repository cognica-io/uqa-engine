//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the session generation selected for an INSERT SELECT sink.
use crate::{session::StatementReadSnapshot, Engine};
use std::rc::Rc;
use uqa_execution::{
    mutation::insert::source::{
        binding::InsertSelectBinding, InsertSelectConsumer, InsertSourceContext,
    },
    query::consumer::{QueryConsumerControl, QueryRowConsumer},
    OwnedPhysicalRow, RowSchema,
};
use uqa_sql::SQLError;

enum SourceSession<'a> {
    Live(&'a Engine),
    Snapshot(Box<Engine>),
}
impl SourceSession<'_> {
    fn engine(&self) -> &Engine {
        match self {
            Self::Live(engine) => engine,
            Self::Snapshot(engine) => engine,
        }
    }
    fn source_context(&self) -> InsertSourceContext<'_, StatementReadSnapshot> {
        let engine = self.engine();
        InsertSourceContext {
            rows: engine.mutation_preparation_context(),
            identities: engine.insert_identity_context(),
            runtime: engine.query_runtime_view(),
        }
    }
}
struct BoundInsertConsumer<'a> {
    session: SourceSession<'a>,
    consumer: Rc<InsertSelectConsumer<StatementReadSnapshot>>,
}
impl QueryRowConsumer for BoundInsertConsumer<'_> {
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        self.consumer
            .begin(self.session.source_context(), columns, schema)
    }
    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        self.consumer.consume(self.session.source_context(), row)
    }
}
impl InsertSelectBinding<StatementReadSnapshot> for Engine {
    fn bind<'a>(
        &'a self,
        snapshot: Option<&StatementReadSnapshot>,
        consumer: Rc<InsertSelectConsumer<StatementReadSnapshot>>,
    ) -> Result<Rc<dyn QueryRowConsumer + 'a>, SQLError> {
        let session = match snapshot {
            Some(snapshot) => {
                SourceSession::Snapshot(Box::new(self.statement_read_snapshot_engine(snapshot)))
            }
            None => SourceSession::Live(self),
        };
        Ok(Rc::new(BoundInsertConsumer { session, consumer }))
    }
}
