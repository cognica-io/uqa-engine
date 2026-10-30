//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated column references retain their supplied name owners.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

impl OwnedScalarExpr {
    /// Construct a generated column reference without copying its admitted name or qualification.
    pub fn column(
        column: Produced<String>,
        qualifier: Option<Produced<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        Self::positioned_column(column, qualifier, None, control)
    }

    /// Preserve the source location when a rewrite changes a column's namespace or physical name.
    pub fn positioned_column(
        column: Produced<String>,
        qualifier: Option<Produced<String>>,
        position: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let (column, memory) = column.into_parts();
        let column = control.finish(column, memory)?;
        let qualifier = qualifier
            .map(|qualifier| {
                let (qualifier, memory) = qualifier.into_parts();
                control.finish(qualifier, memory)
            })
            .transpose()?;
        let (column, column_memory) = column.into_parts();
        let (expression, memory) = match qualifier {
            Some(qualifier) => {
                let (qualifier, qualifier_memory) = qualifier.into_parts();
                (
                    ScalarExpr::QualifiedColumn {
                        qualifier,
                        column: crate::ast::ReferenceName::from(column).with_position(position),
                    },
                    control.combine(column_memory, qualifier_memory),
                )
            }
            None => (
                ScalarExpr::Column(crate::ast::ReferenceName::from(column).with_position(position)),
                column_memory,
            ),
        };
        let output = Self {
            expression,
            memory: memory.map(|direct| ScalarMemory {
                children: BudgetedVec::new(direct.budget()),
                direct,
            }),
        };
        output.rewrite(control, Ok)
    }

    /// Replace only a reference's qualifier. Existing capacity is reused; growth admits a replacement buffer while the original is still live.
    pub fn replace_qualifier(
        mut self,
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let current = self.expression.reference_qualifier_mut().ok_or_else(|| {
            crate::SQLError::Internal("qualifier replacement requires a qualified reference".into())
        })?;
        if qualifier.len() <= current.capacity() {
            current.clear();
            current.push_str(qualifier);
        } else {
            let (value, lease) = control.copy_text(qualifier)?.into_parts();
            let capacity = current.capacity();
            *current = value;
            if let Some(memory) = &mut self.memory {
                drop(memory.direct.split(capacity));
                memory.direct.absorb(lease.expect("controlled qualifier"));
            }
        }
        control.check()?;
        Ok(self)
    }
}

impl OwnedScalarExpr {
    /// Remove only a column reference's qualifier, preserving its name buffer and parser position.
    pub fn remove_reference_qualifier(mut self, control: &ProductionControl<'_>) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::QualifiedColumn { qualifier, column } = std::mem::replace(
            &mut self.expression,
            ScalarExpr::Literal(uqa_core::Value::Null),
        ) else {
            panic!("qualifier removal requires a qualified column")
        };
        let bytes = qualifier.capacity();
        drop(qualifier);
        self.expression = ScalarExpr::Column(column);
        if let Some(memory) = &mut self.memory {
            drop(memory.direct.split(bytes));
        }
        Ok(self)
    }
}
