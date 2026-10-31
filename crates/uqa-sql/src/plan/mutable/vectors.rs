//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replacing a scalar vector transfers each child while admitting the destination buffer and its field records.

use super::{
    Copying, MutableQueryPlan, OwnedScalarExpr, RelationalPlan, RewriteMemory, SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

struct ScalarVector {
    values: Vec<ScalarExpr>,
    memory: Option<RewriteMemory>,
}

impl ScalarVector {
    fn new(
        children: impl ExactSizeIterator<Item = OwnedScalarExpr>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            let values = children
                .map(|child| {
                    Ok(child
                        .finish(control)?
                        .into_uncontrolled()
                        .expect("ordinary scalar vector"))
                })
                .collect::<Result<_, SQLError>>()?;
            return Ok(Self {
                values,
                memory: None,
            });
        };
        let values = copying.map(children, Copying::take_scalar)?;
        let output = Self {
            values,
            memory: Some(copying.into_rewrite_memory()),
        };
        control.check()?;
        Ok(output)
    }
}

impl MutableQueryPlan {
    /// Replace all grouping keys while preserving projection, source and grouping-set owners.
    pub fn replace_group_by(
        mut self,
        expressions: impl ExactSizeIterator<Item = OwnedScalarExpr>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let replacement = ScalarVector::new(expressions, control)?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "grouping keys require a query block".into(),
            ));
        };
        block.group_by = replacement.values;
        if let Some(memory) = &mut self.memory {
            let block = memory
                .field_mut("root")
                .as_mut()
                .expect("query root")
                .field_mut("block")
                .as_mut()
                .expect("query block");
            *block.field_mut("group_by") = replacement.memory;
        }
        control.check()?;
        Ok(self)
    }
}
