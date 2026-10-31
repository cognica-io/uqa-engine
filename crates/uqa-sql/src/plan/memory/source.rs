//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::{AllocationObserver, ObserveAllocations};
use uqa_core::ValueRetentionError;

use crate::plan::{SourcePlan, TableFunctionPlan};

impl ObserveAllocations for TableFunctionPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            name,
            binding,
            output_name,
            relations,
            args,
            column_aliases,
            column_types,
        } = self;
        observer.child(name)?;
        observer.child(binding)?;
        observer.child(output_name)?;
        observer.child(relations)?;
        observer.child(args)?;
        observer.child(column_aliases)?;
        observer.child(column_types)?;
        Ok(())
    }
}

impl ObserveAllocations for SourcePlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::RetainedRows(_) => {}
            Self::Table {
                name,
                executable_query,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                include_descendants: _,
            } => {
                observer.child(name)?;
                observer.child(executable_query)?;
                observer.child(qualifier)?;
                observer.child(alias)?;
                observer.child(column_aliases)?;
                observer.child(bound_columns)?;
            }
            Self::Join {
                comma: _,
                left,
                right,
                kind: _,
                on,
                using,
                natural: _,
                alias,
                column_aliases,
                lateral: _,
                strategy: _,
            } => {
                observer.child(left)?;
                observer.child(right)?;
                observer.child(on)?;
                observer.child(using)?;
                observer.child(alias)?;
                observer.child(column_aliases)?;
            }
            Self::Values {
                lateral: _,
                rows,
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
                internal_column_collations,
            } => {
                observer.child(rows)?;
                observer.child(alias)?;
                observer.child(column_aliases)?;
                observer.child(internal_relation)?;
                observer.child(internal_column_types)?;
                observer.child(internal_column_collations)?;
            }
            Self::Function {
                name,
                binding,
                output_name,
                relations,
                args,
                alias,
                column_aliases,
                ordinality: _,
                column_types,
            } => {
                observer.child(name)?;
                observer.child(binding)?;
                observer.child(output_name)?;
                observer.child(relations)?;
                observer.child(args)?;
                observer.child(alias)?;
                observer.child(column_aliases)?;
                observer.child(column_types)?;
            }
            Self::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality: _,
            } => {
                observer.child(functions)?;
                observer.child(alias)?;
                observer.child(column_aliases)?;
            }
            Self::Subquery {
                captured_output,
                lateral: _,
                body,
                alias,
                column_aliases,
            } => {
                observer.child(captured_output)?;
                observer.child(body)?;
                observer.child(alias)?;
                observer.child(column_aliases)?;
            }
        }
        Ok(())
    }
}
