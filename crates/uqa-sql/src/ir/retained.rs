//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable evaluated constants share storage without retaining every decoded payload.

use std::{fmt, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_core::Value;

use crate::{ast::ColumnType, SQLError};

/// Storage for an already evaluated value. Reading must reproduce the original value without invoking SQL or changing it; implementations own their resource until the final shared handle is dropped.
pub trait ConstantValueSource: fmt::Debug + Send + Sync {
    fn read(&self, index: u64) -> Result<Value, SQLError>;

    /// Borrow a resident immutable value when its owner exposes one. This never reads or decodes a spill record.
    fn resident_value(&self, _index: u64) -> Option<&Value> {
        None
    }

    /// External providers retain their completed output at this boundary. Built-in storage overrides this method to admit decoding before allocation.
    fn read_with_control(
        &self,
        index: u64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        control.check()?;
        match self.resident_value(index) {
            Some(value) => control.copy_value(value).map_err(Into::into),
            None => control
                .retain_external_value(self.read(index)?)
                .map_err(Into::into),
        }
    }

    /// Observe resident owner storage without decoding a constant or touching its spill files. External providers may have no exposed allocation observation.
    fn observe_allocations<'a>(
        &'a self,
        _observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        Ok(())
    }
}

/// An immutable positional value in a shared owner. Copying this handle never reads or copies the payload.
#[derive(Clone)]
pub struct RetainedConstantValue {
    source: Arc<dyn ConstantValueSource>,
    index: u64,
}

impl RetainedConstantValue {
    pub fn new(source: Arc<dyn ConstantValueSource>, index: u64) -> Self {
        Self { source, index }
    }

    pub fn read(&self) -> Result<Value, SQLError> {
        crate::expr::check_statement_cancellation()?;
        let value = self.source.read(self.index)?;
        crate::expr::check_statement_cancellation()?;
        Ok(value)
    }

    /// Move an evaluated value into its immutable shared owner without copying it or dropping its constructor lease.
    pub fn from_produced(
        value: Produced<Value>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self::new(value.into_shared(control)?, 0))
    }

    pub fn resident_value(&self) -> Option<&Value> {
        self.source.resident_value(self.index)
    }

    pub fn read_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        crate::expr::check_statement_cancellation()?;
        control.check()?;
        let value = self.source.read_with_control(self.index, control)?;
        crate::expr::check_statement_cancellation()?;
        control.check()?;
        Ok(value)
    }
}

impl fmt::Debug for RetainedConstantValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedConstantValue")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl PartialEq for RetainedConstantValue {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.source, &other.source) && self.index == other.index {
            return true;
        }
        if let (Some(left), Some(right)) = (self.resident_value(), other.resident_value()) {
            return left.has_same_representation(right);
        }
        // IR representation comparison may conservatively decline a rewrite when a resource cannot be read. SQL evaluation retains the original fallible read and reports its error.
        match (self.read(), other.read()) {
            (Ok(left), Ok(right)) => left.has_same_representation(&right),
            _ => false,
        }
    }
}

impl Serialize for RetainedConstantValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.read()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RetainedConstantValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self::new(Arc::new(OwnedConstant(value)), 0))
    }
}

#[derive(Debug)]
struct OwnedConstant(Value);

impl ConstantValueSource for OwnedConstant {
    fn resident_value(&self, index: u64) -> Option<&Value> {
        (index == 0).then_some(&self.0)
    }
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        observer.allocation(size_of::<Self>(), size_of::<Self>())?;
        observer.child(&self.0)
    }

    fn read(&self, index: u64) -> Result<Value, SQLError> {
        if index != 0 {
            return Err(SQLError::Internal(
                "retained constant index is outside its owner".into(),
            ));
        }
        Ok(self.0.clone())
    }
}

impl ConstantValueSource for Produced<Value> {
    fn resident_value(&self, index: u64) -> Option<&Value> {
        (index == 0).then_some(self)
    }

    fn read(&self, index: u64) -> Result<Value, SQLError> {
        self.resident_value(index).cloned().ok_or_else(|| {
            SQLError::Internal("retained constant index is outside its owner".into())
        })
    }

    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        observer.allocation(size_of::<Self>(), size_of::<Self>())?;
        observer.child(&**self)
    }
}

impl uqa_core::memory::ObserveAllocations for RetainedConstantValue {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        if observer.shared_owner(self.source.as_ref())? {
            self.source.observe_allocations(observer)?;
        }
        Ok(())
    }
}

/// Selected result metadata shared with an already evaluated datum. Plan copies never duplicate its allocation or change its constructor lease.
#[derive(Debug, Clone, PartialEq)]
pub struct RetainedScalarLiteral(Arc<Produced<RetainedLiteralData>>);

#[derive(Debug, PartialEq)]
pub struct RetainedLiteralData {
    pub value: RetainedConstantValue,
    pub ty: Option<ColumnType>,
    pub declaration: Option<String>,
    pub collation_oid: Option<u32>,
}

impl std::ops::Deref for RetainedScalarLiteral {
    type Target = RetainedLiteralData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for RetainedScalarLiteral {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.materialize()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl RetainedScalarLiteral {
    pub fn new(
        value: RetainedConstantValue,
        ty: Option<ColumnType>,
        declaration: Option<String>,
        collation_oid: Option<u32>,
    ) -> Self {
        let control = ProductionControl::uncontrolled();
        Self(
            control
                .finish(
                    RetainedLiteralData {
                        value,
                        ty,
                        declaration,
                        collation_oid,
                    },
                    None,
                )
                .expect("ordinary constant metadata")
                .into_shared(&control)
                .expect("ordinary shared constant metadata"),
        )
    }

    /// The value source retains its own storage lease. Metadata constructors supply their existing leases and this owner admits the shared header before allocation.
    pub fn with_control(
        value: RetainedConstantValue,
        ty: Option<Produced<ColumnType>>,
        declaration: Option<Produced<String>>,
        collation_oid: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (ty, ty_memory) = match ty {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let (declaration, declaration_memory) = match declaration {
            Some(name) => {
                let (name, memory) = name.into_parts();
                (Some(name), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let data = RetainedLiteralData {
            value,
            ty,
            declaration,
            collation_oid,
        };
        Ok(Self(
            control
                .finish(data, control.combine(ty_memory, declaration_memory))?
                .into_shared(control)?,
        ))
    }

    /// Materialize one value only at an interface that requires an owned literal. Cloning plans retains the shared owner instead.
    pub fn materialize(&self) -> Result<crate::ScalarExpr, SQLError> {
        let value = self.value.read()?;
        let Some(declaration) = &self.declaration else {
            return Ok(crate::ScalarExpr::Literal(value));
        };
        Ok(crate::ScalarExpr::TypedLiteral {
            value,
            ty: declaration.clone(),
            bound_type: self.ty.clone(),
            parameter_index: None,
            composite_source: None,
            collation_oid: self.collation_oid,
        })
    }
}

impl uqa_core::memory::ObserveAllocations for RetainedScalarLiteral {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        if observer.shared_owner(self.0.as_ref())? {
            observer.allocation(
                size_of::<Produced<RetainedLiteralData>>(),
                size_of::<Produced<RetainedLiteralData>>(),
            )?;
            observer.child(&self.value)?;
            observer.child(&self.ty)?;
            observer.child(&self.declaration)?;
        }
        Ok(())
    }
}

/// Planner supplies a successfully evaluated constant; execution supplies its bounded storage implementation.
pub trait ConstantValueStorage: fmt::Debug + Send + Sync {
    fn retain(&self, value: Value) -> Result<RetainedConstantValue, SQLError>;
}

#[cfg(test)]
mod tests;
