//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deliver each final RETURNING row before the mutation advances to another row.

use crate::{
    query::consumer::{QueryConsumerControl, QueryRowConsumer},
    OwnedPhysicalRow, RowSchema,
};
use uqa_sql::SQLError;

#[derive(Clone, Copy)]
pub struct ReturningDestination<'a> {
    consumer: &'a dyn QueryRowConsumer,
    schema: &'a RowSchema,
}

impl<'a> ReturningDestination<'a> {
    pub fn new(
        consumer: &'a dyn QueryRowConsumer,
        schema: &'a RowSchema,
    ) -> Result<Self, SQLError> {
        consumer.begin(schema.columns(), schema)?;
        Ok(Self { consumer, schema })
    }

    pub(super) fn consume(self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        if row.schema.columns() != self.schema.columns() {
            return Err(SQLError::Internal(
                "RETURNING values do not match their bound output columns".into(),
            ));
        }
        let values = row.row.project_slots(row.schema.layout_slots());
        self.consumer
            .consume(OwnedPhysicalRow::new(self.schema.clone(), values))
    }
}
