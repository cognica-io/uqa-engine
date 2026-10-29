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
                        column: column.into(),
                    },
                    control.combine(column_memory, qualifier_memory),
                )
            }
            None => (ScalarExpr::Column(column.into()), column_memory),
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
}
