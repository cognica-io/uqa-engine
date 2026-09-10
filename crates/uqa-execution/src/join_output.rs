//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional output shaping for `JOIN ... USING` and `NATURAL JOIN`.

use crate::RowSchemaExecution;
use uqa_core::Value;
use uqa_sql::expr::cast_value;

use crate::{Batch, ColumnIdentity, ExecError, ExecResult, PhysicalOperator, RowSchema};

pub use uqa_sql::schema::join_output::JoinOutputSource;

/// Reorder and merge join columns while retaining the joined physical row.
/// Inner, left, and right joins are schema-only remaps; a full join appends
/// only the merged values that cannot be represented by one existing slot.
pub struct JoinOutput<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    computed: Vec<JoinOutputSource>,
}

impl<'a> JoinOutput<'a> {
    /// Compile the positional schema of a qualified join without constructing or executing an operator. Binders use this to expose the exact same merged-column identities and hidden qualified aliases as execution.
    pub fn try_schema(
        input: &RowSchema,
        columns: &[(String, ColumnIdentity, JoinOutputSource)],
        aliases: &[(ColumnIdentity, JoinOutputSource)],
    ) -> ExecResult<RowSchema> {
        compile_layout(input, columns, aliases)
            .map(|(schema, _)| schema)
            .map_err(Into::into)
    }

    pub fn try_new(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, ColumnIdentity, JoinOutputSource)>,
        aliases: Vec<(ColumnIdentity, JoinOutputSource)>,
    ) -> ExecResult<Self> {
        let (schema, computed) = compile_layout(child.row_schema(), &columns, &aliases)?;
        Ok(Self {
            child,
            schema,
            computed,
        })
    }
}

pub use uqa_sql::schema::join_output::compile_layout;

impl PhysicalOperator for JoinOutput<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.child.estimated_cardinality()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        if self.computed.is_empty() {
            return Ok(Some(Batch::from_physical_rows(
                self.schema.clone(),
                batch.rows,
            )));
        }
        let rows = batch
            .rows
            .into_iter()
            .map(|row| {
                let values = {
                    let view = batch.schema.view(&row);
                    self.computed
                        .iter()
                        .map(|source| evaluate_source(source, &view))
                        .collect::<ExecResult<Vec<_>>>()?
                };
                Ok(row.append_values(values))
            })
            .collect::<ExecResult<Vec<_>>>()?;
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

pub use uqa_sql::schema::join_output::source_type;

fn evaluate_source(
    source: &JoinOutputSource,
    view: &crate::PhysicalRowView<'_>,
) -> ExecResult<Value> {
    match source {
        JoinOutputSource::Input(position) => {
            Ok(view.value_at(*position).unwrap_or(&Value::Null).clone())
        }
        JoinOutputSource::Cast { input, ty } => cast_value(
            view.value_at(*input).unwrap_or(&Value::Null),
            &ty.sql_name(),
        )
        .map_err(ExecError::from),
        JoinOutputSource::Coalesce { left, right, ty } => {
            let target = ty.sql_name();
            let left = cast_value(view.value_at(*left).unwrap_or(&Value::Null), &target)?;
            if !matches!(left, Value::Null) {
                return Ok(left);
            }
            cast_value(view.value_at(*right).unwrap_or(&Value::Null), &target)
                .map_err(ExecError::from)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::RowSchemaExecution;
    use std::collections::BTreeMap;
    use uqa_sql::ast::ColumnType;

    use super::*;
    use crate::physical::run_to_rows;
    use crate::TableScan;
    use uqa_sql::expr::RowLookup;

    #[test]
    fn schema_only_merge_reuses_input_slots_and_keeps_qualified_aliases() {
        let child = TableScan::from_rows(
            vec![
                "l.id".into(),
                "l.name".into(),
                "r.id".into(),
                "r.note".into(),
            ],
            vec![BTreeMap::from([
                ("l.id".into(), Value::Int(1)),
                ("l.name".into(), Value::Str("left".into())),
                ("r.id".into(), Value::Int(1)),
                ("r.note".into(), Value::Str("right".into())),
            ])],
        );
        let mut output = JoinOutput::try_new(
            Box::new(child),
            vec![
                (
                    "id".into(),
                    ColumnIdentity::unqualified("id"),
                    JoinOutputSource::Input(0),
                ),
                (
                    "name".into(),
                    ColumnIdentity::qualified("l", "name"),
                    JoinOutputSource::Input(1),
                ),
                (
                    "note".into(),
                    ColumnIdentity::qualified("r", "note"),
                    JoinOutputSource::Input(3),
                ),
            ],
            vec![
                (
                    ColumnIdentity::qualified("l", "id"),
                    JoinOutputSource::Input(0),
                ),
                (
                    ColumnIdentity::qualified("r", "id"),
                    JoinOutputSource::Input(2),
                ),
            ],
        )
        .unwrap();
        output.open().unwrap();
        let batch = output.next().unwrap().unwrap();
        assert_eq!(batch.schema.columns(), ["id", "name", "note"]);
        let view = batch.schema.view(&batch.rows[0]);
        assert_eq!(view.qualified_column("l", "id"), Some(&Value::Int(1)));
        assert_eq!(view.qualified_column("r", "id"), Some(&Value::Int(1)));
        output.close().unwrap();
    }

    #[test]
    fn full_merge_coalesces_only_the_requested_column() {
        let child = TableScan::from_rows(
            vec!["l.id".into(), "r.id".into()],
            vec![BTreeMap::from([
                ("l.id".into(), Value::Null),
                ("r.id".into(), Value::Int(3)),
            ])],
        );
        let mut output = JoinOutput::try_new(
            Box::new(child),
            vec![(
                "id".into(),
                ColumnIdentity::unqualified("id"),
                JoinOutputSource::Coalesce {
                    left: 0,
                    right: 1,
                    ty: ColumnType::Integer,
                },
            )],
            Vec::new(),
        )
        .unwrap();
        let (_, rows) = run_to_rows(&mut output).unwrap();
        assert_eq!(rows[0]["id"], Value::Int(3));
    }
}
