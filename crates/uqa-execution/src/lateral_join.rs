//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated physical join operator.

use uqa_sql::ast::JoinKind;
use uqa_sql::ResultRow;

use crate::batch::DEFAULT_BATCH_SIZE;
use crate::{
    Batch, ExecError, ExecResult, OwnedPhysicalRow, PhysicalOperator, PhysicalRow, RowSchema,
};

/// Engine seam for a correlated right-hand relation.
///
/// The physical operator owns join iteration, `ON` filtering, and outer-row
/// preservation. The engine callback only evaluates the right relation and
/// scalar predicate in the current correlated scope.
pub type LateralRows = Box<dyn Iterator<Item = ExecResult<OwnedPhysicalRow>> + Send>;

pub trait LateralSource: Send {
    fn rows_for(&mut self, left: &OwnedPhysicalRow) -> ExecResult<LateralRows>;

    fn matches(&mut self, joined: &OwnedPhysicalRow) -> ExecResult<bool>;
}

fn output_schema(
    left: &RowSchema,
    right: &RowSchema,
    left_nulls: &ResultRow,
    right_nulls: &ResultRow,
) -> RowSchema {
    RowSchema::join(
        left,
        right,
        left_nulls.keys().chain(right_nulls.keys()).cloned(),
    )
}

/// Streaming physical implementation of a SQL `LATERAL` join.
///
/// The left child is pulled in batches. The correlated right relation is
/// evaluated once per left row, and produced rows are drained in bounded
/// output batches instead of materialising the complete join result.
pub struct LateralJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    source: Box<dyn LateralSource + 'a>,
    kind: JoinKind,
    schema: RowSchema,
    left_schema: RowSchema,
    right_schema: RowSchema,
    left_rows: std::vec::IntoIter<OwnedPhysicalRow>,
    current_left: Option<OwnedPhysicalRow>,
    right_rows: Option<LateralRows>,
    matched_left: bool,
    exhausted: bool,
}

impl<'a> LateralJoin<'a> {
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        source: Box<dyn LateralSource + 'a>,
        kind: JoinKind,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
    ) -> Self {
        let right_schema = RowSchema::new(right_nulls.keys().cloned().collect());
        Self::new_with_right_schema(left, source, kind, left_nulls, right_nulls, right_schema)
    }

    pub fn new_with_right_schema(
        left: Box<dyn PhysicalOperator + 'a>,
        source: Box<dyn LateralSource + 'a>,
        kind: JoinKind,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        right_schema: RowSchema,
    ) -> Self {
        let schema = output_schema(left.row_schema(), &right_schema, &left_nulls, &right_nulls);
        let left_schema = left.row_schema().clone();
        Self {
            left,
            source,
            kind,
            schema,
            left_schema,
            right_schema,
            left_rows: Vec::new().into_iter(),
            current_left: None,
            right_rows: None,
            matched_left: false,
            exhausted: false,
        }
    }

    fn next_left(&mut self) -> ExecResult<Option<OwnedPhysicalRow>> {
        loop {
            if let Some(row) = self.left_rows.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.left.next()? else {
                return Ok(None);
            };
            self.left_rows = batch.into_owned_rows().into_iter();
        }
    }

    fn begin_left_row(&mut self, left: OwnedPhysicalRow) -> ExecResult<()> {
        self.right_rows = Some(self.source.rows_for(&left)?);
        self.current_left = Some(left);
        self.matched_left = false;
        Ok(())
    }
}

impl PhysicalOperator for LateralJoin<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.left_rows = Vec::new().into_iter();
        self.current_left = None;
        self.right_rows = None;
        self.matched_left = false;
        self.exhausted = false;
        self.left.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.exhausted && self.current_left.is_none() {
            return Ok(None);
        }

        let mut output = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        while output.len() < DEFAULT_BATCH_SIZE {
            if self.current_left.is_none() {
                match self.next_left()? {
                    Some(left) => self.begin_left_row(left)?,
                    None => {
                        self.exhausted = true;
                        break;
                    }
                }
            }

            let next_right = self
                .right_rows
                .as_mut()
                .and_then(Iterator::next)
                .transpose()?;
            if let Some(right) = next_right {
                let left = self.current_left.as_ref().ok_or_else(|| {
                    ExecError::Other(
                        "lateral join produced a right row without a current left row".into(),
                    )
                })?;
                let joined = OwnedPhysicalRow::new(
                    self.schema.clone(),
                    PhysicalRow::concat(&left.row, &right.row),
                );
                let matched =
                    matches!(self.kind, JoinKind::Cross) || self.source.matches(&joined)?;
                if matched {
                    self.matched_left = true;
                    output.push(joined.row);
                } else if matches!(self.kind, JoinKind::Right | JoinKind::Full) {
                    output.push(PhysicalRow::concat(
                        &PhysicalRow::nulls(self.left_schema.physical_width()),
                        &right.row,
                    ));
                }
                continue;
            }

            self.right_rows = None;
            if !self.matched_left && matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                let left = self.current_left.take().ok_or_else(|| {
                    ExecError::Other(
                        "lateral join completed a right stream without a current left row".into(),
                    )
                })?;
                output.push(PhysicalRow::concat(
                    &left.row,
                    &PhysicalRow::nulls(self.right_schema.physical_width()),
                ));
            } else {
                self.current_left = None;
            }
        }

        if output.is_empty() {
            return Ok(None);
        }
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), output)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.left_rows = Vec::new().into_iter();
        self.current_left = None;
        self.right_rows = None;
        self.matched_left = false;
        self.exhausted = true;
        self.left.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;
    use uqa_core::Value;

    fn row(values: &[(&str, Value)]) -> ResultRow {
        values
            .iter()
            .map(|(column, value)| ((*column).to_string(), value.clone()))
            .collect()
    }

    fn right_row(value: i64) -> OwnedPhysicalRow {
        let schema = RowSchema::new(vec!["r.n".into()]);
        let row = PhysicalRow::from_values(vec![Value::Int(value)]);
        OwnedPhysicalRow::new(schema, row)
    }

    struct RangeSource;

    impl LateralSource for RangeSource {
        fn rows_for(&mut self, left: &OwnedPhysicalRow) -> ExecResult<LateralRows> {
            let Value::Int(end) = left.get("l.n").cloned().unwrap_or(Value::Null) else {
                return Ok(Box::new(std::iter::empty()));
            };
            Ok(Box::new((1..=end).map(|value| Ok(right_row(value)))))
        }

        fn matches(&mut self, joined: &OwnedPhysicalRow) -> ExecResult<bool> {
            Ok(joined.get("r.n") == Some(&Value::Int(2)))
        }
    }

    #[test]
    fn left_lateral_preserves_a_left_row_without_an_on_match() {
        let left = TableScan::from_rows(
            vec!["l.n".into()],
            vec![
                row(&[("l.n", Value::Int(1))]),
                row(&[("l.n", Value::Int(2))]),
            ],
        );
        let mut join = LateralJoin::new(
            Box::new(left),
            Box::new(RangeSource),
            JoinKind::Left,
            row(&[("l.n", Value::Null)]),
            row(&[("r.n", Value::Null)]),
        );
        let (_, rows) = run_to_rows(&mut join).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("l.n"), Some(&Value::Int(1)));
        assert_eq!(rows[0].get("r.n"), Some(&Value::Null));
        assert_eq!(rows[1].get("l.n"), Some(&Value::Int(2)));
        assert_eq!(rows[1].get("r.n"), Some(&Value::Int(2)));
    }

    struct LargeRangeSource;

    impl LateralSource for LargeRangeSource {
        fn rows_for(&mut self, _left: &OwnedPhysicalRow) -> ExecResult<LateralRows> {
            Ok(Box::new((0..10_000).map(|value| Ok(right_row(value)))))
        }

        fn matches(&mut self, _joined: &OwnedPhysicalRow) -> ExecResult<bool> {
            Ok(true)
        }
    }

    #[test]
    fn large_correlated_relation_is_pulled_one_output_batch_at_a_time() {
        let left = TableScan::from_rows(vec!["l.n".into()], vec![row(&[("l.n", Value::Int(1))])]);
        let mut join = LateralJoin::new(
            Box::new(left),
            Box::new(LargeRangeSource),
            JoinKind::Cross,
            row(&[("l.n", Value::Null)]),
            row(&[("r.n", Value::Null)]),
        );
        join.open().unwrap();
        assert_eq!(join.next().unwrap().unwrap().len(), DEFAULT_BATCH_SIZE);
        assert_eq!(join.next().unwrap().unwrap().len(), DEFAULT_BATCH_SIZE);
        join.close().unwrap();
    }

    struct FailingSource;

    impl LateralSource for FailingSource {
        fn rows_for(&mut self, _left: &OwnedPhysicalRow) -> ExecResult<LateralRows> {
            Ok(Box::new(
                vec![
                    Ok(right_row(1)),
                    Err(crate::ExecError::Other("injected lateral failure".into())),
                ]
                .into_iter(),
            ))
        }

        fn matches(&mut self, _joined: &OwnedPhysicalRow) -> ExecResult<bool> {
            Ok(true)
        }
    }

    #[test]
    fn late_correlated_source_error_is_propagated() {
        let left = TableScan::from_rows(vec!["l.n".into()], vec![row(&[("l.n", Value::Int(1))])]);
        let mut join = LateralJoin::new(
            Box::new(left),
            Box::new(FailingSource),
            JoinKind::Cross,
            ResultRow::new(),
            ResultRow::new(),
        );
        join.open().unwrap();
        let error = join.next().unwrap_err();
        assert!(error.to_string().contains("injected lateral failure"));
    }
}
