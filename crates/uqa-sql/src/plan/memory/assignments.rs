//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{
    AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets,
    WindowDefinition,
};
use uqa_core::memory::{AllocationObserver, ObserveAllocations};
use uqa_core::ValueRetentionError;

impl<E: ObserveAllocations> ObserveAllocations for AssignmentTargets<E> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Single(target) => observer.child(target),
            Self::Multiple(group) => observer.child(group),
        }
    }
}

impl<E: ObserveAllocations> ObserveAllocations for AssignmentTarget<E> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            column,
            indirection,
        } = self;
        observer.child(column)?;
        observer.child(indirection)
    }
}

impl<E: ObserveAllocations> ObserveAllocations for MultipleAssignmentTargets<E> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            targets,
            source_positions,
            source_width: _,
        } = self;
        observer.child(targets)?;
        observer.child(source_positions)
    }
}

impl<E: ObserveAllocations> ObserveAllocations for AssignmentStep<E> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Field(name) => observer.child(name),
            Self::Index(expression) => observer.child(expression),
            Self::Slice { lower, upper } => {
                observer.child(lower)?;
                observer.child(upper)
            }
        }
    }
}

impl<S: ObserveAllocations> ObserveAllocations for WindowDefinition<S> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            name,
            spec,
            inherited: _,
        } = self;
        observer.child(name)?;
        observer.child(spec)
    }
}
