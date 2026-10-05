//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate explicitly queued foreign keys after every rewritten relation is published.

use std::cell::RefCell;
use uqa_sql::{ast::ForeignKey, SQLError};

#[derive(Default)]
pub struct DeferredForeignKeys(RefCell<Vec<(String, String, ForeignKey)>>);

impl DeferredForeignKeys {
    pub fn retain(&self, table: &str, name: &str, key: &ForeignKey) {
        self.0
            .borrow_mut()
            .push((table.to_string(), name.to_string(), key.clone()));
    }

    pub fn validate(
        &self,
        context: crate::mutation::constraints::context::ConstraintContext<'_>,
    ) -> Result<(), SQLError> {
        for (table, name, key) in std::mem::take(&mut *self.0.borrow_mut()) {
            crate::schema::validation::validate_foreign_key_rows(context, &table, &name, &key)?;
        }
        Ok(())
    }
}
