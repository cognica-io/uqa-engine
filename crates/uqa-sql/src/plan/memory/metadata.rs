//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::{AllocationObserver, ObserveAllocations};
use uqa_core::ValueRetentionError;

use crate::ast::{
    AssignmentBinding, AssignmentPathStep, ColumnType, CompositeFieldBinding, CompositeRowBinding,
    CompositeTypeReference, EnumTypeReference, FunctionBinding, FunctionResolutionError,
    FunctionValueBinding, InternalRelationId, IntervalFields, JoinUsing, LockWait, LockingClause,
    OperatorJoinRelations, OperatorResolutionError, RangeSubtype, RangeTypeReference,
    ReturningAliases, RoutineInvocationBinding,
};
use crate::expr::composites::{
    CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor, DroppedCompositeAttribute,
};

// Copy bounds make these inline-only observations fail to compile if a carrier acquires owned storage.
macro_rules! inline {
    ($($ty:ty),* $(,)?) => {$ (
        impl ObserveAllocations for $ty {
            fn observe_allocations<'a>(&'a self, _: &mut AllocationObserver<'a, '_>) -> Result<(), ValueRetentionError> {
                fn require_copy<T: Copy>(_: &T) {}
                require_copy(self);
                Ok(())
            }
        }
    )*};
}
inline!(
    AssignmentPathStep,
    IntervalFields,
    RangeSubtype,
    DroppedCompositeAttribute,
    LockWait,
    InternalRelationId
);

impl ObserveAllocations for RangeTypeReference {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            schema,
            name,
            oid: _,
            array_oid: _,
            alignment: _,
        } = self;
        observer.child(schema)?;
        observer.child(name)
    }
}

impl ObserveAllocations for FunctionBinding {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            object_id: _,
            name,
            argument_types,
            builtin: _,
            dispatch: _,
            invocation,
            value_binding,
            resolution_error,
            input_collation: _,
        } = self;
        observer.child(name)?;
        observer.child(argument_types)?;
        observer.child(invocation)?;
        observer.child(value_binding)?;
        observer.child(resolution_error)?;
        Ok(())
    }
}

impl ObserveAllocations for FunctionValueBinding {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::CompositeField(value0) => {
                observer.child(value0)?;
            }
            Self::Assignment(value0) => {
                observer.child(value0)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for CompositeFieldBinding {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            type_oid: _,
            number: _,
            result_type,
            collation_oid: _,
            dropped: _,
            changed_type,
        } = self;
        observer.child(result_type)?;
        observer.child(changed_type)?;
        Ok(())
    }
}

impl ObserveAllocations for FunctionResolutionError {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::UndefinedFunction { signature } => {
                observer.child(signature)?;
            }
            Self::Operator(value0) => {
                observer.child(value0)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for OperatorResolutionError {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { sqlstate, message } = self;
        observer.child(sqlstate)?;
        observer.child(message)?;
        Ok(())
    }
}

impl ObserveAllocations for RoutineInvocationBinding {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            argument_positions,
            argument_targets,
            argument_sources,
            parameter_types,
            return_type,
            variadic_mode: _,
        } = self;
        observer.child(argument_positions)?;
        observer.child(argument_targets)?;
        observer.child(argument_sources)?;
        observer.child(parameter_types)?;
        observer.child(return_type)?;
        Ok(())
    }
}

impl ObserveAllocations for AssignmentBinding {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            path,
            value_argument: _,
        } = self;
        observer.child(path)?;
        Ok(())
    }
}

impl ObserveAllocations for ColumnType {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Named(value0) => {
                observer.child(value0)?;
            }
            Self::SmallInteger => {}
            Self::Integer => {}
            Self::BigInteger => {}
            Self::Oid => {}
            Self::Xid => {}
            Self::Boolean => {}
            Self::Void => {}
            Self::Text => {}
            Self::RefCursor => {}
            Self::Name => {}
            Self::Uuid => {}
            Self::Varchar(_) => {}
            Self::Bpchar => {}
            Self::Character(_) => {}
            Self::Real => {}
            Self::DoublePrecision => {}
            Self::Numeric {
                precision: _,
                scale: _,
            } => {}
            Self::Json => {}
            Self::Xml => {}
            Self::JsonB => {}
            Self::Bytea => {}
            Self::Bit(_) => {}
            Self::VarBit(_) => {}
            Self::InternalChar => {}
            Self::Regproc => {}
            Self::Regprocedure => {}
            Self::Regclass => {}
            Self::Regnamespace => {}
            Self::Regcollation => {}
            Self::Regrole => {}
            Self::Regtype => {}
            Self::PgNodeTree => {}
            Self::AclItem => {}
            Self::Int2Vector => {}
            Self::OidVector => {}
            Self::AnyArray => {}
            Self::Record => {}
            Self::Array(value0) => {
                observer.child(value0)?;
            }
            Self::Date => {}
            Self::Time => {}
            Self::TimePrecision(_) => {}
            Self::TimeTz => {}
            Self::TimeTzPrecision(_) => {}
            Self::Timestamp => {}
            Self::TimestampPrecision(_) => {}
            Self::TimestampTz => {}
            Self::TimestampTzPrecision(_) => {}
            Self::Interval => {}
            Self::IntervalWithFields {
                fields,
                precision: _,
            } => {
                observer.child(fields)?;
            }
            Self::Range(value0) => {
                observer.child(value0)?;
            }
            Self::Multirange(value0) => {
                observer.child(value0)?;
            }
            Self::DeclaredRange(value0) => {
                observer.child(value0)?;
            }
            Self::DeclaredMultirange(value0) => {
                observer.child(value0)?;
            }
            Self::Vector(_) => {}
            Self::Tensor(_) => {}
            Self::Domain {
                schema,
                name,
                oid: _,
                array_oid: _,
                collation_oid: _,
                base,
            } => {
                observer.child(schema)?;
                observer.child(name)?;
                observer.child(base)?;
            }
            Self::Enum(value0) => {
                observer.child(value0)?;
            }
            Self::Composite(value0) => {
                observer.child(value0)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for EnumTypeReference {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            schema,
            name,
            oid: _,
            array_oid: _,
        } = self;
        observer.child(schema)?;
        observer.child(name)?;
        Ok(())
    }
}

impl ObserveAllocations for CompositeTypeReference {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            schema,
            name,
            oid: _,
            array_oid: _,
            relation_oid: _,
        } = self;
        observer.child(schema)?;
        observer.child(name)?;
        Ok(())
    }
}

impl ObserveAllocations for CompositeRowBinding {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            ty,
            attributes,
            layout: _,
            argument_types,
        } = self;
        observer.child(ty)?;
        observer.child(attributes)?;
        observer.child(argument_types)?;
        Ok(())
    }
}

impl ObserveAllocations for CompositeConstantSource {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { value, descriptors } = self;
        observer.child(value)?;
        observer.child(descriptors)?;
        Ok(())
    }
}

impl ObserveAllocations for CompositeTypeDescriptor {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            dropped,
            type_oid: _,
            relation_oid: _,
            attributes,
        } = self;
        observer.child(dropped)?;
        observer.child(attributes)?;
        Ok(())
    }
}

impl ObserveAllocations for CompositeAttribute {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            name,
            ty,
            collation_oid: _,
            number: _,
        } = self;
        observer.child(name)?;
        observer.child(ty)?;
        Ok(())
    }
}

impl ObserveAllocations for JoinUsing {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { columns, alias } = self;
        observer.child(columns)?;
        observer.child(alias)?;
        Ok(())
    }
}

impl ObserveAllocations for OperatorJoinRelations {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { left, right } = self;
        observer.child(left)?;
        observer.child(right)?;
        Ok(())
    }
}

impl ObserveAllocations for ReturningAliases {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            old,
            new,
            old_explicit: _,
            new_explicit: _,
        } = self;
        observer.child(old)?;
        observer.child(new)?;
        Ok(())
    }
}

impl ObserveAllocations for LockingClause {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            strength: _,
            wait,
            relations,
        } = self;
        observer.child(wait)?;
        observer.child(relations)?;
        Ok(())
    }
}
