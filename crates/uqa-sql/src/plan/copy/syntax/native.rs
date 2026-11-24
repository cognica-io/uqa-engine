//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain native routine declaration fields with the existing SQL syntax projections and constructor driver.

use super::{Copying, Result, SyntaxCopy};
use crate::{ast, construction, plan::copy::CopyPurpose};
use std::future::Future;
use uqa_core::memory::{MemoryError, Produced, ProductionControl};

mod collections;
mod compact;
pub(crate) use compact::Window;

pub(crate) struct Input<'s> {
    copying: Copying<'s>,
    pub(crate) work: construction::Work<'s>,
    depth: usize,
    maximum_depth: usize,
    slots: usize,
}

pub(crate) struct Nested<T> {
    pub(crate) value: Produced<T>,
    pub(crate) children: usize,
    pub(crate) has_merge_targets: bool,
    depth: usize,
    slots: usize,
    compact: bool,
}

impl<T> Nested<T> {
    pub(crate) const fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn retirement_depth(&self) -> usize {
        if !self.compact && (self.depth > Window::STEPS || self.slots > Window::STEPS) {
            self.depth
        } else {
            0
        }
    }
}

pub(crate) trait Field: Sized + Send + Sync {
    /// An opaque payload goes through its native constructor until its complete copy cost is known.
    fn fits(&self, _: &mut Window) -> bool {
        false
    }

    fn capture_inline(&self, _: &ProductionControl<'_>) -> Result<Option<Nested<Self>>> {
        Ok(None)
    }

    fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> impl Future<Output = Result<Self>> + Send;
}

impl<'s> Input<'s> {
    pub(crate) fn new(control: ProductionControl<'s>, work: construction::Work<'s>) -> Self {
        Self {
            copying: Copying::capture(control, CopyPurpose::Stored),
            work,
            depth: 1,
            maximum_depth: 1,
            slots: 0,
        }
    }
    pub(super) fn inline<T: SyntaxCopy>(&mut self, value: &T) -> Result<T> {
        value.copy_syntax(&mut self.copying)
    }

    pub(crate) fn control(&self) -> ProductionControl<'s> {
        self.copying.control
    }

    pub(crate) async fn field<T: Field>(&mut self, value: &'s T) -> Result<T> {
        self.depth += 1;
        self.maximum_depth = self.maximum_depth.max(self.depth);
        let field = value.copy_from(self).await;
        self.depth -= 1;
        field
    }

    pub(crate) async fn list<T: Field>(&mut self, source: &'s [T]) -> Result<Vec<T>> {
        self.slots = self.slots.saturating_add(source.len());
        let mut values = self.copying.reserve_slots(source.len())?;
        for value in source {
            self.work.checkpoint().await;
            values.push(self.field(value).await?);
        }
        Ok(values)
    }

    pub(crate) fn finish<T>(self, value: T) -> Result<(Produced<T>, usize)> {
        let children = self.copying.retained_children;
        Ok((self.copying.finish(value)?, children))
    }

    pub(crate) fn finish_nested<T>(self, value: T) -> Result<Nested<T>> {
        let has_merge_targets = self.copying.has_merge_targets;
        let depth = self.maximum_depth;
        let slots = self.slots;
        let (value, children) = self.finish(value)?;
        Ok(Nested {
            value,
            children,
            has_merge_targets,
            compact: false,
            depth,
            slots,
        })
    }

    pub(crate) fn take_nested<T>(&mut self, child: Nested<T>) -> Result<T> {
        self.maximum_depth = self.maximum_depth.max(self.depth + child.depth - 1);
        self.slots = self.slots.saturating_add(child.slots).saturating_add(1);
        self.copying.has_merge_targets |= child.has_merge_targets;
        self.take_child((child.value, child.children))
    }

    pub(crate) fn observe_merge(&mut self, statement: &ast::MergeStmt) {
        self.copying.has_merge_targets |= !statement.target_column_bindings.is_empty();
    }

    pub(crate) fn take_child<T>(&mut self, (value, children): (Produced<T>, usize)) -> Result<T> {
        self.copying.retained_children = self
            .copying
            .retained_children
            .checked_add(children)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(self.copying.take(value))
    }
}

macro_rules! inline_fields {
    ($($ty:ty),* $(,)?) => { $(
        impl Field for $ty {
            fn fits(&self, _: &mut Window) -> bool { true }
            fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> impl Future<Output = Result<Self>> + Send {
                std::future::ready(input.inline(self))
            }
        }
    )* };
}

inline_fields!(
    ast::BinaryOp,
    ast::FunctionOrderSyntax,
    ast::FunctionParamMode,
    ast::InternalColumnRef,
    ast::NullsOrder,
    ast::WindowCallModifiers,
    ast::WindowReferenceKind,
    ast::RetainedRowSource,
    ast::CompositeRowLayout,
    ast::InternalRelationId,
    ast::SetOpKind,
    ast::CteMaterialization,
    ast::JoinKind,
    ast::LockStrength,
    ast::LockWait,
    ast::FrameMode,
    ast::FrameExclusion,
    ast::OverridingKind,
    bool,
    usize,
    u16,
    u32,
    i16,
    [u8; 16]
);

impl Field for ast::RetainedInputLiteral {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let (literal, bytes) = self
            .0
            .capture_with_footprint_from(&input.control(), &mut input.work)
            .await?;
        input.copying.retained_children = input
            .copying
            .retained_children
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(Self(literal))
    }
}

impl Field for crate::ScalarExpr {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        if matches!(
            self,
            Self::Star
                | Self::Default
                | Self::Param(_)
                | Self::Position(_)
                | Self::InternalColumn(_)
                | Self::Literal(
                    uqa_core::Value::Void
                        | uqa_core::Value::Null
                        | uqa_core::Value::Bool(_)
                        | uqa_core::Value::Int(_)
                        | uqa_core::Value::Float(_)
                )
        ) {
            return input.copying.scalar(self);
        }
        let control = input.control();
        let value = construction::retain_with(control, || {
            self.capture_with_footprint_from(&control, &mut input.work)
        })?
        .await?;
        input.take_child(value)
    }
}

impl Field for crate::ColumnIdentity {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let value = Self::copy_from_work(
            self.qualifier(),
            self.column(),
            &input.control(),
            &mut input.work,
        )
        .await?;
        Ok(input.copying.take(value))
    }
}

impl Field for crate::catalog::roles::RoleReference {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        Ok(match self {
            Self::Named(name) => Self::Named(input.field(name).await?),
            Self::Bound(owner) => Self::Bound(owner.clone()),
        })
    }
}

impl Field for ast::ReturningAliases {
    fn fits(&self, window: &mut Window) -> bool {
        window.text(&self.old) && window.text(&self.new)
    }
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        crate::plan::copy::lowering::input::text::returning_aliases(
            self,
            &mut input.copying,
            &mut input.work,
        )
        .await
    }
}

/// Select the same native fields to measure or construct a stored SQL body statement.
macro_rules! body_statements {
    ($($variant:ident $(($item:ident))? $({$field:ident})?),+ $(,)?) => {
        impl Field for ast::Statement {
            fn fits(&self, window: &mut Window) -> bool {
                match self {
                    $(Self::$variant $(($item))? $({$field})? => window.field($($item)? $($field)?),)+
                    _ => false,
                }
            }

            fn capture_inline(&self, control: &ProductionControl<'_>) -> Result<Option<Nested<Self>>> {
                control.check()?;
                let mut window = Window::new();
                if !window.field(self) {
                    return Ok(None);
                }
                let (value, (children, has_merge_targets)) =
                    crate::plan::copy::capture_stored(self, control, |copy| {
                        (copy.retained_children, copy.has_merge_targets)
                    })?;
                Ok(Some(Nested { value, children, has_merge_targets, depth: window.maximum_depth(), slots: 0, compact: true }))
            }

            async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
                Ok(match self {
                    $(Self::$variant $(($item))? $({$field})? =>
                        Self::$variant $((input.field($item).await?))? $({$field: input.field($field).await?})?,)+
                    utility => input.inline(utility)?,
                })
            }
        }
    };
}
body_statements!(
    Select(query),
    Values { rows },
    Insert(statement),
    Update(statement),
    Delete(statement),
    Merge(statement),
);

impl Field for crate::plan::CommandPlan {
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        Ok(match self {
            Self::Insert(plan) => Self::Insert(input.field(plan).await?),
            Self::Update(plan) => Self::Update(input.field(plan).await?),
            Self::Delete(plan) => Self::Delete(input.field(plan).await?),
            Self::Merge(plan) => Self::Merge(input.field(plan).await?),
            utility => input.inline(utility)?,
        })
    }
}

macro_rules! payload {
    ($ty:ty, $method:ident $(, |$source:ident, $window:ident| $bounded:expr)?) => {
        impl Field for $ty {
            $(fn fits(&self, $window: &mut Window) -> bool { let $source = self; $bounded })?
            async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
                let value = self.$method(&input.control(), &mut input.work).await?;
                Ok(input.copying.take(value))
            }
        }
    };
}
payload!(
    uqa_core::Value,
    clone_from_input,
    |value, window| match value {
        uqa_core::Value::Void
        | uqa_core::Value::Null
        | uqa_core::Value::Int(_)
        | uqa_core::Value::Bool(_)
        | uqa_core::Value::Float(_) => true,
        uqa_core::Value::Str(text) => window.text(text),
        _ => false,
    }
);
payload!(ast::FunctionBinding, clone_with_input_work);
payload!(crate::ColumnType, clone_with_input_work);
payload!(
    crate::expr::composites::CompositeTypeDescriptor,
    clone_with_input_work
);

impl Field for String {
    fn fits(&self, window: &mut Window) -> bool {
        window.text(self)
    }
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let value =
            crate::catalog::work::copy_text(&mut input.work, self, &input.copying.control).await?;
        Ok(input.copying.take(value))
    }
}
impl Field for ast::ReferenceName {
    fn fits(&self, window: &mut Window) -> bool {
        window.text(self.as_str())
    }
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        let text =
            crate::catalog::work::copy_text(&mut input.work, self.as_str(), &input.copying.control)
                .await?;
        Ok(Self::from(input.copying.take(text)).with_position(self.position()))
    }
}
impl<T: Field> Field for Option<T> {
    fn fits(&self, window: &mut Window) -> bool {
        self.as_ref().is_none_or(|value| window.field(value))
    }
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        match self {
            Some(value) => Ok(Some(input.field(value).await?)),
            None => Ok(None),
        }
    }
}
impl<T: Field> Field for Box<T> {
    fn fits(&self, window: &mut Window) -> bool {
        window.field(self.as_ref())
    }
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        input.copying.admit_box::<T>()?;
        let child = input.field(self.as_ref()).await?;
        input.copying.finish_box(child, std::convert::identity)
    }
}
impl<T: Field> Field for Vec<T> {
    fn fits(&self, window: &mut Window) -> bool {
        window.slots(self.len()) && self.iter().all(|value| window.field(value))
    }
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        input.list(self).await
    }
}
impl<A: Field, B: Field> Field for (A, B) {
    fn fits(&self, window: &mut Window) -> bool {
        window.field(&self.0) && window.field(&self.1)
    }
    async fn copy_from<'s>(&'s self, input: &mut Input<'s>) -> Result<Self> {
        Ok((input.field(&self.0).await?, input.field(&self.1).await?))
    }
}
