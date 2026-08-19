//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming one-to-many projection (`ProjectSet`).

use uqa_sql::ResultRow;

use crate::batch::DEFAULT_BATCH_SIZE;
use crate::{Batch, ExecResult, OwnedPhysicalRow, PhysicalOperator, PhysicalRow, RowSchema};

/// Owned row stream produced for one input row.
///
/// The iterator yields errors at the exact row where production failed. This
/// matters for generators backed by files or user code: eagerly collecting a
/// `Vec` both defeated the Volcano memory boundary and made late failures
/// impossible to represent without discarding already-produced rows.
pub type ProjectRows = Box<dyn Iterator<Item = ExecResult<ResultRow>> + Send>;

/// Engine seam for set-returning projection expressions.
pub trait SetProjector: Send {
    fn project(&mut self, row: &ResultRow) -> ExecResult<ProjectRows>;
}

impl<F> SetProjector for F
where
    F: FnMut(&ResultRow) -> ExecResult<ProjectRows> + Send,
{
    fn project(&mut self, row: &ResultRow) -> ExecResult<ProjectRows> {
        self(row)
    }
}

/// Pulls input rows and expands each through a set-returning projection.
pub struct ProjectSet<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    projector: Box<dyn SetProjector + 'a>,
    schema: RowSchema,
    input: std::vec::IntoIter<ResultRow>,
    projected: Option<ProjectRows>,
    exhausted: bool,
}

impl<'a> ProjectSet<'a> {
    pub fn new(
        child: Box<dyn PhysicalOperator + 'a>,
        output_schema: Vec<String>,
        projector: Box<dyn SetProjector + 'a>,
    ) -> Self {
        Self {
            child,
            projector,
            schema: RowSchema::new(output_schema),
            input: Vec::new().into_iter(),
            projected: None,
            exhausted: false,
        }
    }

    fn next_input(&mut self) -> ExecResult<Option<ResultRow>> {
        loop {
            if let Some(row) = self.input.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            self.input = batch.into_result_rows().into_iter();
        }
    }
}

impl PhysicalOperator for ProjectSet<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.projected = None;
        self.exhausted = false;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.exhausted && self.projected.is_none() {
            return Ok(None);
        }

        let mut rows = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        while rows.len() < DEFAULT_BATCH_SIZE {
            if let Some(projected) = self.projected.as_mut() {
                match projected.next() {
                    Some(row) => {
                        rows.push(row?);
                        continue;
                    }
                    None => self.projected = None,
                }
            }

            match self.next_input()? {
                Some(row) => self.projected = Some(self.projector.project(&row)?),
                None => {
                    self.exhausted = true;
                    break;
                }
            }
        }
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(Batch::new(self.schema.clone(), rows)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.projected = None;
        self.exhausted = true;
        self.child.close()
    }
}

/// Owned physical row stream produced for one input row without materializing named maps.
pub type PhysicalProjectRows = Box<dyn Iterator<Item = ExecResult<PhysicalRow>> + Send>;

/// Engine seam for set-returning projections that keep rows in their physical layout.
pub trait PhysicalSetProjector: Send {
    fn project(&mut self, row: OwnedPhysicalRow) -> ExecResult<PhysicalProjectRows>;
}

impl<F> PhysicalSetProjector for F
where
    F: FnMut(OwnedPhysicalRow) -> ExecResult<PhysicalProjectRows> + Send,
{
    fn project(&mut self, row: OwnedPhysicalRow) -> ExecResult<PhysicalProjectRows> {
        self(row)
    }
}

/// Pulls physical input rows and expands each without crossing the named-row materialization boundary.
pub struct PhysicalProjectSet<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    projector: Box<dyn PhysicalSetProjector + 'a>,
    schema: RowSchema,
    input: std::vec::IntoIter<OwnedPhysicalRow>,
    projected: Option<PhysicalProjectRows>,
    exhausted: bool,
}

impl<'a> PhysicalProjectSet<'a> {
    pub fn new(
        child: Box<dyn PhysicalOperator + 'a>,
        schema: RowSchema,
        projector: Box<dyn PhysicalSetProjector + 'a>,
    ) -> Self {
        Self {
            child,
            projector,
            schema,
            input: Vec::new().into_iter(),
            projected: None,
            exhausted: false,
        }
    }

    fn next_input(&mut self) -> ExecResult<Option<OwnedPhysicalRow>> {
        loop {
            if let Some(row) = self.input.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            self.input = batch.into_owned_rows().into_iter();
        }
    }
}

impl PhysicalOperator for PhysicalProjectSet<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.projected = None;
        self.exhausted = false;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.exhausted && self.projected.is_none() {
            return Ok(None);
        }

        let mut rows = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        while rows.len() < DEFAULT_BATCH_SIZE {
            if let Some(projected) = self.projected.as_mut() {
                match projected.next() {
                    Some(row) => {
                        rows.push(row?);
                        continue;
                    }
                    None => self.projected = None,
                }
            }

            match self.next_input()? {
                Some(row) => self.projected = Some(self.projector.project(row)?),
                None => {
                    self.exhausted = true;
                    break;
                }
            }
        }
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.projected = None;
        self.exhausted = true;
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;
    use crate::{RowLockOrigin, RowProjectionValue};
    use uqa_core::Value;

    #[test]
    fn expands_each_input_row_in_child_order() {
        let input: ResultRow = [("n".into(), Value::Int(2))].into_iter().collect();
        let child = TableScan::from_rows(vec!["n".into()], vec![input]);
        let projector = |row: &ResultRow| -> ExecResult<ProjectRows> {
            let Value::Int(end) = row.get("n").cloned().unwrap_or(Value::Null) else {
                return Ok(Box::new(std::iter::empty()));
            };
            Ok(Box::new((1..=end).map(|value| {
                Ok([("value".into(), Value::Int(value))].into_iter().collect())
            })))
        };
        let mut project =
            ProjectSet::new(Box::new(child), vec!["value".into()], Box::new(projector));
        let (_, rows) = run_to_rows(&mut project).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("value"), Some(&Value::Int(1)));
        assert_eq!(rows[1].get("value"), Some(&Value::Int(2)));
    }

    #[test]
    fn one_input_with_many_outputs_never_builds_an_unbounded_pending_queue() {
        let input: ResultRow = [("n".into(), Value::Int(10_000))].into_iter().collect();
        let child = TableScan::from_rows(vec!["n".into()], vec![input]);
        let projector = |row: &ResultRow| -> ExecResult<ProjectRows> {
            let Value::Int(end) = row.get("n").cloned().unwrap_or(Value::Null) else {
                return Ok(Box::new(std::iter::empty()));
            };
            Ok(Box::new((0..end).map(|value| {
                Ok([("value".into(), Value::Int(value))].into_iter().collect())
            })))
        };
        let mut project =
            ProjectSet::new(Box::new(child), vec!["value".into()], Box::new(projector));
        project.open().unwrap();
        let first = project.next().unwrap().unwrap();
        assert_eq!(first.len(), DEFAULT_BATCH_SIZE);
        let second = project.next().unwrap().unwrap();
        assert_eq!(second.len(), DEFAULT_BATCH_SIZE);
        project.close().unwrap();
    }

    #[test]
    fn late_projector_error_is_not_converted_to_end_of_stream() {
        let child = TableScan::from_rows(vec!["n".into()], vec![ResultRow::new()]);
        let projector = |_row: &ResultRow| -> ExecResult<ProjectRows> {
            Ok(Box::new(
                vec![
                    Ok([("value".into(), Value::Int(1))].into_iter().collect()),
                    Err(crate::ExecError::Other("injected projector failure".into())),
                ]
                .into_iter(),
            ))
        };
        let mut project =
            ProjectSet::new(Box::new(child), vec!["value".into()], Box::new(projector));
        project.open().unwrap();
        let error = project.next().unwrap_err();
        assert!(error.to_string().contains("injected projector failure"));
    }

    #[test]
    fn physical_projection_preserves_row_lineage_without_named_materialization() {
        let input_schema = RowSchema::new(vec!["unused".into(), "value".into()]);
        let input_row = PhysicalRow::from_values(vec![
            Value::Str("unused payload".repeat(64)),
            Value::Str("shared value".repeat(64)),
        ])
        .with_lock_origin(RowLockOrigin::new("source", "public.source", 7));
        let child = TableScan::from_physical_rows(input_schema, vec![input_row]);
        let projector = |row: OwnedPhysicalRow| -> ExecResult<PhysicalProjectRows> {
            let slot = row.schema.physical_slot(1).unwrap();
            Ok(Box::new(std::iter::once(Ok(row.row.project_with_values(
                [RowProjectionValue::InputSlot(slot)],
            )))))
        };
        let output_schema = RowSchema::new(vec!["value".into()]);
        let mut project =
            PhysicalProjectSet::new(Box::new(child), output_schema.clone(), Box::new(projector));

        project.open().unwrap();
        let output = project.next().unwrap().unwrap();
        assert_eq!(output.rows.len(), 1);
        assert_eq!(
            output_schema.view(&output.rows[0]).value_at(0),
            Some(&Value::Str("shared value".repeat(64)))
        );
        assert_eq!(output.rows[0].lock_origins()[0].doc_id, 7);
        assert!(project.next().unwrap().is_none());
        project.close().unwrap();
    }
}
