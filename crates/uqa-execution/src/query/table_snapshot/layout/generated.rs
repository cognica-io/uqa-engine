//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Missing generated fields borrow only their selected expression inputs.

use super::RowLayout;
use crate::query::generated::{evaluate_generated_expression, prepare_generated_column};
use crate::query::table_snapshot::documents::projection::visit_source_projection;
use uqa_core::{
    memory::{BudgetedMap, BudgetedVec},
    DocId, Value,
};
use uqa_sql::{ast::ColumnDef, expr::RowLookup, SQLError};
use uqa_storage::{DocumentStore, StorageBackendResult};

struct ProjectedInput<'a> {
    slots: &'a BudgetedMap<&'a str, usize>,
    values: &'a [&'a Value],
}

impl RowLookup for ProjectedInput<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        self.slots.get(name).map(|index| self.values[*index])
    }

    fn qualified_column(&self, _: &str, _: &str) -> Option<&Value> {
        None
    }
}

impl RowLayout {
    pub(super) fn generated_field(
        &self,
        source: &dyn DocumentStore,
        id: DocId,
        column: &ColumnDef,
    ) -> StorageBackendResult<Option<Value>> {
        self.control.check()?;
        let present = source.contains_doc_id(id)?;
        self.control.check()?;
        if !present {
            return Ok(None);
        }
        let schema = crate::RowSchema::with_types(
            self.columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            self.columns
                .iter()
                .map(|column| Some(column.ty.clone()))
                .collect(),
        );
        let expression = prepare_generated_column(
            &schema,
            column.generated.as_ref().expect("selected generated field"),
        )
        .map_err(Self::error)?;
        let mut names = BudgetedVec::new(self.control.memory());
        let mut slots = BudgetedMap::new(self.control.memory());
        let projectable = expression.try_visit_columns(&mut |name| {
            self.control.check()?;
            if !slots.contains_key(name) {
                names.reserve(1)?;
                slots.insert(name, names.len())?;
                names.push(name)?;
            }
            Ok::<_, uqa_storage::StorageBackendError>(())
        })?;
        if !projectable {
            return Err(Self::error(SQLError::Internal(
                "validated generated expression requires a relational row shape".into(),
            )));
        }
        let projection = self.generated_inputs(&names)?;
        let mut output = None;
        let read = visit_source_projection(
            &self.control,
            source,
            &[id],
            &projection,
            &[],
            &mut |_, present, values| {
                if present {
                    let row = ProjectedInput {
                        slots: &slots,
                        values,
                    };
                    output = Some(
                        evaluate_generated_expression(&expression, &row)
                            .and_then(|value| {
                                uqa_sql::assignment::conversion::convert_value_to_column_type(
                                    value, &column.ty,
                                )
                            })
                            .map_err(Self::error),
                    );
                }
                false
            },
        );
        // Keep the evaluator's first typed failure if the provider observes a later cancellation while returning.
        let output = output.transpose()?;
        read?;
        self.control.check()?;
        Ok(output)
    }
}
