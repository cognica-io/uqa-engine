//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional output shaping for `JOIN ... USING` and `NATURAL JOIN`.

use uqa_core::Value;

use crate::{Batch, ExecError, ExecResult, PhysicalOperator, RowSchema};

/// Source of one visible or hidden join-output identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinOutputSource {
    /// Reuse an existing logical input position without copying its value.
    Input(usize),
    /// SQL `COALESCE(left, right)` over two logical input positions. This is
    /// required only for a merged column of `FULL JOIN`.
    Coalesce { left: usize, right: usize },
}

/// Reorder and merge join columns while retaining the joined physical row.
/// Inner, left, and right joins are schema-only remaps; a full join appends
/// only the merged values that cannot be represented by one existing slot.
pub struct JoinOutput<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    coalesce: Vec<(usize, usize)>,
}

impl<'a> JoinOutput<'a> {
    pub fn try_new(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, JoinOutputSource)>,
        aliases: Vec<(String, JoinOutputSource)>,
    ) -> ExecResult<Self> {
        let input_width = child.row_schema().len();
        let mut coalesce = Vec::<(usize, usize)>::new();
        for source in columns
            .iter()
            .map(|(_, source)| source)
            .chain(aliases.iter().map(|(_, source)| source))
        {
            match source {
                JoinOutputSource::Input(position) if *position >= input_width => {
                    return Err(ExecError::Other(format!(
                        "join output input position {position} is outside width {input_width}"
                    )));
                }
                JoinOutputSource::Coalesce { left, right }
                    if *left >= input_width || *right >= input_width =>
                {
                    return Err(ExecError::Other(format!(
                        "join output coalesce positions ({left}, {right}) are outside width {input_width}"
                    )));
                }
                JoinOutputSource::Coalesce { left, right } => {
                    if !coalesce.contains(&(*left, *right)) {
                        coalesce.push((*left, *right));
                    }
                }
                JoinOutputSource::Input(_) => {}
            }
        }

        let computed_names = (0..coalesce.len())
            .map(|index| format!("\0uqa.join_using.{index}"))
            .collect::<Vec<_>>();
        let intermediate = RowSchema::append(child.row_schema(), &computed_names);
        let source_position = |source: JoinOutputSource| -> usize {
            match source {
                JoinOutputSource::Input(position) => position,
                JoinOutputSource::Coalesce { left, right } => {
                    let index = coalesce
                        .iter()
                        .position(|pair| pair == &(left, right))
                        .expect("coalesce source was registered");
                    intermediate
                        .position(&computed_names[index])
                        .expect("computed join output column exists")
                }
            }
        };
        let columns = columns
            .into_iter()
            .map(|(name, source)| (name, source_position(source)))
            .collect::<Vec<_>>();
        let aliases = aliases
            .into_iter()
            .map(|(name, source)| (name, source_position(source)))
            .collect::<Vec<_>>();
        let schema = RowSchema::remap_positions(&intermediate, &columns, &aliases);
        Ok(Self {
            child,
            schema,
            coalesce,
        })
    }
}

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
        if self.coalesce.is_empty() {
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
                    self.coalesce
                        .iter()
                        .map(|(left, right)| {
                            let left = view.value_at(*left).unwrap_or(&Value::Null);
                            if matches!(left, Value::Null) {
                                view.value_at(*right).unwrap_or(&Value::Null).clone()
                            } else {
                                left.clone()
                            }
                        })
                        .collect()
                };
                row.append_values(values)
            })
            .collect();
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

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
                ("id".into(), JoinOutputSource::Input(0)),
                ("name".into(), JoinOutputSource::Input(1)),
                ("note".into(), JoinOutputSource::Input(3)),
            ],
            vec![
                ("l.id".into(), JoinOutputSource::Input(0)),
                ("r.id".into(), JoinOutputSource::Input(2)),
            ],
        )
        .unwrap();
        output.open().unwrap();
        let batch = output.next().unwrap().unwrap();
        assert_eq!(batch.schema.columns(), ["id", "name", "note"]);
        let view = batch.schema.view(&batch.rows[0]);
        assert_eq!(
            view.qualified_column("l", "id", "l.id"),
            Some(&Value::Int(1))
        );
        assert_eq!(
            view.qualified_column("r", "id", "r.id"),
            Some(&Value::Int(1))
        );
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
                JoinOutputSource::Coalesce { left: 0, right: 1 },
            )],
            Vec::new(),
        )
        .unwrap();
        let (_, rows) = run_to_rows(&mut output).unwrap();
        assert_eq!(rows[0]["id"], Value::Int(3));
    }
}
