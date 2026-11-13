//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored query constructors share scalar input admission and retain native children without evaluating their expressions.

use super::{
    stored_input::{invalid, Input, InputError, Values},
    AssignmentStep, AssignmentTarget, AssignmentTargets, ColumnType, CteBody, CteCycleClause,
    CteMaterialization, CteSearchClause, DeleteStmt, Expr, FrameBound, FrameExclusion, FrameMode,
    FromClause, FunctionBinding, InsertStmt, InternalRelationId, JoinKind, JoinUsing, LockStrength,
    LockWait, LockingClause, MergeStmt, MergeTargetColumnBinding, MergeWhen,
    MultipleAssignmentTargets, NullsOrder, OnConflict, OnConflictAction, OperatorJoinRelations,
    OrderBy, OverridingKind, Projection, ReturningAliases, SelectStmt, SetOp, SetOpKind,
    TableFunction, UpdateStmt, WindowCallModifiers, WindowDefinition, WindowFrame, WindowReference,
    WindowReferenceKind, WindowSpec, CTE,
};
use uqa_core::{json::JsonToken, memory::Produced};

#[macro_use]
mod fields;
mod assignments;
mod mutations;
mod queries;
mod sources;
#[cfg(test)]
mod tests;

trait Stored: Sized {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError>;
}

mod construction;
pub(in crate::ast) use construction::Native;
use construction::Parts;

impl<'a> Input<'a, '_> {
    fn stored<T: Stored>(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<T>, InputError> {
        T::read(self, token, values)
    }

    pub(in crate::ast) fn query_payload(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Expr>, InputError> {
        match tag {
            "ScalarSubquery" => {
                let mut parts = Parts::new(self.control);
                let body = parts.take(self.stored(token, values)?);
                parts.finish(Expr::ScalarSubquery(body))
            }
            "Exists" => fields!(
                self,
                token,
                values,
                Expr::Exists {
                    body: required,
                    negated: required
                }
            ),
            "InSubquery" => fields!(
                self,
                token,
                values,
                Expr::InSubquery {
                    expr: required,
                    body: required,
                    negated: required
                }
            ),
            "WindowCall" => fields!(
                self,
                token,
                values,
                Expr::WindowCall {
                    name: required,
                    args: required,
                    spec: required,
                    filter: default(None),
                    modifiers: default(WindowCallModifiers::default())
                }
            ),
            _ => Err(invalid()),
        }
    }
}

impl Stored for String {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        _: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        Native::new(input.text(token)?, input.control)
    }
}
impl Stored for bool {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        _: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        Native::new(
            input.scalar(super::stored_input::boolean(token)?)?,
            input.control,
        )
    }
}
impl Stored for Expr {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        input.query_expression(token, values)
    }
}
impl Stored for FunctionBinding {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        _: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        Native::new(input.function_binding(token)?, input.control)
    }
}
impl Stored for ColumnType {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        _: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        Native::new(input.column(token)?, input.control)
    }
}
impl<T: Stored> Stored for Option<T> {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        if token == JsonToken::Null {
            return Parts::new(input.control).finish(None);
        }
        let (value, memory) = input.stored::<T>(token, values)?.into_parts();
        Native::new(input.control.finish(Some(value), memory)?, input.control)
    }
}
impl<T: Stored> Stored for Vec<T> {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let mut output = uqa_core::memory::ProductionVec::new(input.control);
        let mut cursor = input.array_cursor(token)?;
        while let Some(token) = cursor.next(input)? {
            output.push_produced(input.stored(token, values)?.into())?;
        }
        Native::new(output.finish()?, input.control)
    }
}
impl<T: Stored> Stored for Box<T> {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let value = input.stored::<T>(token, values)?;
        let allocation = input.control.reserve(size_of::<T>())?;
        let (value, memory) = value.into_parts();
        Native::new(
            input
                .control
                .finish(Box::new(value), input.control.combine(memory, allocation))?,
            input.control,
        )
    }
}
impl<A: Stored, B: Stored> Stored for (A, B) {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        if token != JsonToken::StartArray {
            return Err(invalid());
        }
        let mut first = None;
        let mut second = None;
        let mut cursor = input.field_cursor(token, &["0", "1"], [3, 3], false)?;
        while let Some((index, token)) = cursor.next(input)? {
            if index == 0 {
                first = Some(input.stored(token, values)?);
            } else {
                second = Some(input.stored(token, values)?);
            }
        }
        let mut parts = Parts::new(input.control);
        let result = (
            parts.take(first.ok_or_else(invalid)?),
            parts.take(second.ok_or_else(invalid)?),
        );
        parts.finish(result)
    }
}

macro_rules! numbers {
    ($($ty:ty),* $(,)?) => {$(impl Stored for $ty {
        fn read<'a>(input: &mut Input<'a, '_>, token: JsonToken<'a>, _: &Values<'_>) -> Result<Native<Self>, InputError> {
            Native::new(input.scalar(super::stored_input::number(token)?)?, input.control)
        }
    })*};
}
numbers!(u32, u64, usize, i16);

macro_rules! units {
    ($($ty:ty),* $(,)?) => {$(impl Stored for $ty {
        fn read<'a>(input: &mut Input<'a, '_>, token: JsonToken<'a>, _: &Values<'_>) -> Result<Native<Self>, InputError> {
            let tag = input.unit_tag(token)?;
            Native::new(input.scalar(crate::ast::stored_input::unit(&tag)?)?, input.control)
        }
    })*};
}
units!(
    NullsOrder,
    FrameMode,
    FrameExclusion,
    WindowReferenceKind,
    CteMaterialization,
    SetOpKind,
    JoinKind,
    LockStrength,
    LockWait,
    OverridingKind
);

impl Stored for InternalRelationId {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        _: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        Native::new(
            input.scalar(Self::from_raw(super::stored_input::number(token)?))?,
            input.control,
        )
    }
}

mod declarations;
