//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! VALUES roots transfer existing row storage into projections or a correlated relational source.

use super::{
    Copying, MutableQueryPlan, MutableSourcePlan, OwnedProjection, OwnedScalarExpr, RelationalPlan,
    RewriteMemory, ScalarQueryArena,
};
use crate::{ast::InternalRelationId, SQLError, ScalarExpr};
use uqa_core::memory::ProductionControl;

pub(in crate::plan) struct OwnedRows {
    pub(in crate::plan) rows: Vec<Vec<ScalarExpr>>,
    pub(in crate::plan) memory: Option<RewriteMemory>,
}

pub struct OwnedValueRow {
    values: std::vec::IntoIter<ScalarExpr>,
    memory: Option<RewriteMemory>,
    position: usize,
}

impl Iterator for OwnedValueRow {
    type Item = OwnedScalarExpr;
    fn next(&mut self) -> Option<Self::Item> {
        let expression = self.values.next()?;
        let memory = self.memory.as_mut().map(|memory| {
            let mut item = memory.item_mut(self.position).take().expect("VALUES item");
            match item.scalar_mut().take().expect("VALUES scalar") {
                RewriteMemory::Scalar(memory) => memory,
                RewriteMemory::Fields { .. } => unreachable!("VALUES scalar owner"),
            }
        });
        self.position += 1;
        Some(OwnedScalarExpr::from_rewrite_parts(expression, memory))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl ExactSizeIterator for OwnedValueRow {}

impl OwnedRows {
    pub(in crate::plan) fn take_row(&mut self, index: usize) -> OwnedValueRow {
        OwnedValueRow {
            values: std::mem::take(&mut self.rows[index]).into_iter(),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| memory.item_mut(index).take()),
            position: 0,
        }
    }
}

impl MutableQueryPlan {
    /// Transform VALUES rows through independently owned scalars while keeping the query namespace and metadata unchanged.
    pub fn map_value_rows<I: ExactSizeIterator<Item = OwnedScalarExpr>>(
        mut self,
        control: &ProductionControl<'_>,
        mut map: impl FnMut(OwnedValueRow) -> Result<I, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::Values { rows, .. } = &mut self.query.root else {
            return Err(SQLError::Internal(
                "VALUES row edit requires a VALUES root".into(),
            ));
        };
        let mut input = OwnedRows {
            rows: std::mem::take(rows),
            memory: self.memory.as_mut().and_then(|memory| {
                memory
                    .field_mut("root")
                    .as_mut()
                    .expect("VALUES root")
                    .field_mut("rows")
                    .take()
            }),
        };
        let (rows, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let rows = copying.map(0..input.rows.len(), |copying, index| {
                copying.map(map(input.take_row(index))?, Copying::take_scalar)
            })?;
            (rows, Some(copying.into_rewrite_memory()))
        } else {
            let rows = (0..input.rows.len())
                .map(|index| {
                    map(input.take_row(index))?
                        .map(|value| {
                            Ok(value
                                .finish(control)?
                                .into_uncontrolled()
                                .expect("ordinary VALUES scalar"))
                        })
                        .collect::<Result<Vec<_>, SQLError>>()
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            (rows, None)
        };
        let RelationalPlan::Values { rows: output, .. } = &mut self.query.root else {
            unreachable!()
        };
        *output = rows;
        if let Some(owner) = &mut self.memory {
            *owner
                .field_mut("root")
                .as_mut()
                .expect("VALUES root")
                .field_mut("rows") = memory;
        }
        control.check()?;
        Ok(self)
    }

    /// Keep the original query scope and scalar arena while exposing VALUES as an ordinary projection block. A single row becomes the projection list; multiple rows retain their buffers in a lateral captured source.
    pub fn project_values(mut self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        if !matches!(self.query.root, RelationalPlan::Values { .. }) {
            return Ok(self);
        }
        let root = std::mem::replace(
            &mut self.query.root,
            RelationalPlan::Values {
                rows: Vec::new(),
                subqueries: Vec::new(),
            },
        );
        let RelationalPlan::Values { rows, subqueries } = root else {
            unreachable!()
        };
        let mut memory = self
            .memory
            .as_mut()
            .and_then(|memory| memory.field_mut("root").take());
        let mut rows = OwnedRows {
            rows,
            memory: memory
                .as_mut()
                .and_then(|memory| memory.field_mut("rows").take()),
        };
        let arena = ScalarQueryArena {
            queries: subqueries,
            memory: memory
                .as_mut()
                .and_then(|memory| memory.field_mut("subqueries").take()),
        };
        drop(memory);
        let mut generated = if rows.rows.len() == 1 {
            let row = rows.take_row(0);
            drop(rows);
            Self::projected(
                row.map(|expression| OwnedProjection {
                    expression,
                    alias: None,
                }),
                control,
            )?
        } else {
            let relation = InternalRelationId::allocate();
            let width = rows.rows.first().map_or(0, Vec::len);
            let mut projections = crate::production::Buffer::new(control);
            for position in 0..width {
                projections.push(
                    OwnedProjection {
                        expression: ScalarExpr::InternalColumn(relation.column(position))
                            .copy_for_rewrite(control)?,
                        alias: None,
                    },
                    control,
                )?;
            }
            let query = Self::projected(projections.into_iter(), control)?;
            let source = if let Some(mut copying) = Copying::for_rewrite(*control) {
                let source = copying.captured_rows_source(rows, relation, width)?;
                MutableSourcePlan {
                    source,
                    memory: Some(copying.into_rewrite_memory()),
                }
            } else {
                MutableSourcePlan {
                    source: crate::plan::SourcePlan::Values {
                        lateral: true,
                        rows: rows.rows,
                        alias: None,
                        column_aliases: Vec::new(),
                        internal_relation: Some(relation),
                        internal_column_types: vec![None; width],
                        internal_column_collations: vec![None; width],
                    },
                    memory: None,
                }
            };
            query.replace_source(Some(source), control)?
        };
        std::mem::swap(&mut self.query.root, &mut generated.query.root);
        if let Some(memory) = &mut self.memory {
            *memory.field_mut("root") = generated
                .memory
                .as_mut()
                .expect("generated root owner")
                .field_mut("root")
                .take();
        }
        self.restore_arena(arena);
        Ok(self)
    }
}
