//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project bound query fields using the stored image's native field selections and the SQL constructor work owner.

pub(crate) use super::syntax::native::{Field, Input, Nested, Window};
use crate::construction;
use crate::plan::{
    retirement::{
        construction::{Context, Prefix, Root},
        Node,
    },
    ExecutableMemory, OwnedExecutablePlan, UnifiedPlan,
};
use uqa_core::memory::{Produced, ProductionControl};

pub(crate) type Captured<T> = (Produced<T>, usize);
pub(crate) type CaptureResult<T> = Result<T, crate::SQLError>;
pub(crate) use super::Copying;

/// Inline and suspended constructors select the same stored fields. A ready caller can use its original inline scratch and exact allowance.
pub(crate) trait Inline: Sized {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self>;
}
pub(crate) fn check(input: &Copying<'_>) -> CaptureResult<()> {
    input.check()
}
macro_rules! syntax_leaf {
    ($($ty:ty),+ $(,)?) => {$(
        impl Inline for $ty {
            fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
                super::syntax::SyntaxCopy::copy_syntax(self, input)
            }
        }
    )+};
}
syntax_leaf!(
    bool,
    usize,
    u16,
    u32,
    i16,
    [u8; 16],
    String,
    crate::ast::Expr,
    crate::ast::OrderBy,
    crate::ast::WindowFrame,
    crate::ast::WindowReference,
    crate::ast::InternalRelationId,
    crate::ast::SetOpKind,
    crate::ast::CteMaterialization,
    crate::ast::JoinKind,
    crate::ast::NullsOrder,
    crate::ast::LockStrength,
    crate::ast::LockWait,
    crate::ast::FrameMode,
    crate::ast::FrameExclusion,
    crate::ast::OverridingKind,
    crate::ColumnType
);

impl Inline for crate::ast::FunctionBinding {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
        let value = self.clone_with_control(&input.control)?;
        Ok(input.take(value))
    }
}

impl<T: Inline> Inline for Option<T> {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
        self.as_ref().map(|value| value.project(input)).transpose()
    }
}
impl<T: Inline> Inline for Vec<T> {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
        input.map(self.iter(), |input, value| value.project(input))
    }
}
impl<T: Inline> Inline for Box<T> {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
        input.boxed(|input| self.as_ref().project(input))
    }
}
impl<T: Inline + Ord> Inline for uqa_core::memory::OwnedSet<T> {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
        input.set(self, |input, value| value.project(input))
    }
}
macro_rules! selected_leaf {
    ($($ty:ty => $method:ident),+ $(,)?) => {$(
        impl Inline for $ty {
            fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> { input.$method(self) }
        }
    )+};
}
selected_leaf!(crate::ScalarExpr => scalar, crate::ColumnIdentity => identity, crate::ast::ReturningAliases => returning_aliases);
impl Inline for crate::catalog::roles::RoleReference {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
        Ok(input.role(Some(self))?.expect("selected privilege subject"))
    }
}

impl Inline for crate::plan::CommandPlan {
    fn project(&self, input: &mut Copying<'_>) -> CaptureResult<Self> {
        Ok(match self {
            Self::Insert(plan) => Self::Insert(plan.project(input)?),
            Self::Update(plan) => Self::Update(plan.project(input)?),
            Self::Delete(plan) => Self::Delete(plan.project(input)?),
            Self::Merge(plan) => Self::Merge(plan.project(input)?),
            utility => super::syntax::SyntaxCopy::copy_syntax(utility, input)?,
        })
    }
}

/// Stored-name and parameter binding edit scalar/metadata fields of the original structural tree. Its original constructor already admitted the frames needed to retire it.
pub(crate) struct OriginalWorkspace(pub(in crate::plan) Context);

/// The same constructor workspace keeps a before-binding shape and its independently owned field receipt until syntax position matching has completed.
macro_rules! binding_syntax {
    ($name:ident, $ty:ty, $kind:ident) => {
        pub(crate) struct $name {
            plan: Original<$ty>,
            memory: Option<uqa_core::memory::MemoryReservation>,
        }

        impl $name {
            fn from_context(value: Produced<$ty>, workspace: Option<&Context>) -> Self {
                let (value, memory) = value.into_parts();
                Self {
                    plan: Original::new(value, workspace),
                    memory,
                }
            }

            pub(crate) async fn retire<W: crate::catalog::work::Work>(
                self,
                control: &ProductionControl<'_>,
                work: &mut W,
            ) -> CaptureResult<()> {
                if let Original::Native(plan) = self.plan {
                    let mut retirement = plan.context().prepare_retirement(control)?;
                    retirement.$kind(plan.take());
                    retirement.retire_from(control, work).await?;
                }
                drop(self.memory);
                Ok(())
            }
        }

        impl std::ops::Deref for $name {
            type Target = $ty;
            fn deref(&self) -> &Self::Target {
                &self.plan
            }
        }
    };
}
binding_syntax!(DefaultExpression, crate::plan::ExpressionPlan, expression);
binding_syntax!(StatementQuery, crate::plan::QueryPlan, query);

impl DefaultExpression {
    pub(crate) fn new(
        value: Produced<crate::plan::ExpressionPlan>,
        workspace: Option<&OriginalWorkspace>,
    ) -> Self {
        Self::from_context(value, workspace.map(|workspace| &workspace.0))
    }
}

/// This borrow ends before the original statement can be retired. Only a live sibling plan owns a second reference to its workspace.
pub(crate) struct StatementWorkspace<'a>(Option<&'a Context>);

impl StatementWorkspace<'_> {
    pub(crate) fn query(&self, value: Produced<crate::plan::QueryPlan>) -> StatementQuery {
        StatementQuery::from_context(value, self.0)
    }
}

impl AsMut<crate::plan::QueryPlan> for StatementQuery {
    fn as_mut(&mut self) -> &mut crate::plan::QueryPlan {
        &mut self.plan
    }
}

pub(crate) struct BoundPlan {
    original: Original<UnifiedPlan>,
    // Release the bound statement before its allocation receipt.
    memory: ExecutableMemory,
}

enum Original<T: Root> {
    Native(Prefix<T>),
    Ordinary(T),
}

impl<T: Root> Original<T> {
    fn new(value: T, workspace: Option<&Context>) -> Self {
        match workspace {
            Some(workspace) => Self::Native(workspace.retain(value)),
            None => Self::Ordinary(value),
        }
    }
}

impl<T: Root> std::ops::Deref for Original<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Self::Native(value) => value,
            Self::Ordinary(value) => value,
        }
    }
}
impl<T: Root> std::ops::DerefMut for Original<T> {
    fn deref_mut(&mut self) -> &mut T {
        match self {
            Self::Native(value) => value,
            Self::Ordinary(value) => value,
        }
    }
}

impl BoundPlan {
    pub(crate) fn new(plan: OwnedExecutablePlan, workspace: Option<OriginalWorkspace>) -> Self {
        let (plan, memory) = plan.into_parts();
        let original = Original::new(plan, workspace.as_ref().map(|workspace| &workspace.0));
        Self { original, memory }
    }

    pub(crate) fn retain_storage(&mut self, storage: ExecutableMemory) {
        self.memory.absorb(storage);
    }

    pub(crate) fn binding_parts(&mut self) -> (&mut UnifiedPlan, StatementWorkspace<'_>) {
        match &mut self.original {
            Original::Native(plan) => {
                let (plan, context) = plan.binding_parts();
                (plan, StatementWorkspace(Some(context)))
            }
            Original::Ordinary(plan) => (plan, StatementWorkspace(None)),
        }
    }

    pub(crate) fn into_ordinary(self) -> UnifiedPlan {
        match self.original {
            Original::Native(plan) => plan.take(),
            Original::Ordinary(plan) => plan,
        }
    }

    pub(crate) async fn retire<W: crate::catalog::work::Work>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> CaptureResult<()> {
        if let Original::Native(owned) = self.original {
            let context = owned.context().clone();
            let node = match owned.take() {
                UnifiedPlan::Query(query) => Node::Query(*query),
                UnifiedPlan::Command(command) => Node::Command(*command),
            };
            context
                .completed_tree(node)
                .retire_from(control, work)
                .await?;
        }
        drop(self.memory);
        Ok(())
    }
}

impl std::ops::Deref for BoundPlan {
    type Target = UnifiedPlan;
    fn deref(&self) -> &Self::Target {
        &self.original
    }
}

impl std::ops::DerefMut for BoundPlan {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.original
    }
}

macro_rules! projection {
    (inline, $ty:ty, $source:ident, $input:ident, $value:expr $(; $window:ident => $fits:expr)?) => {
        impl $crate::plan::copy::stored_plan::Field for $ty {
            $(fn fits(&self, $window: &mut $crate::plan::copy::stored_plan::Window) -> bool {
                let $source=self;
                $fits
            })?
            async fn copy_from<'s>(
                &'s self,
                $input: &mut $crate::plan::copy::stored_plan::Input<'s>,
            ) -> $crate::plan::copy::stored_plan::CaptureResult<Self> {
                let $source = self;
                Ok($value)
            }
        }
    };
    (recursive, $ty:ty, $source:ident, $input:ident, $value:expr $(; $window:ident => $fits:expr)?) => {
        impl $ty {
            fn submit_stored_projection<'s>(
                &'s self,
                control: uqa_core::memory::ProductionControl<'s>,
                work: &mut $crate::construction::Work<'s>,
            ) -> $crate::plan::copy::stored_plan::CaptureResult<
                $crate::construction::Output<
                    $crate::plan::copy::stored_plan::CaptureResult<
                        $crate::plan::copy::stored_plan::Nested<Self>,
                    >,
                >,
            > {
                Ok(work.submit(move |work| async move {
                    let mut $input = $crate::plan::copy::stored_plan::Input::new(control, work);
                    let $source = self;
                    let value = $value;
                    $input.finish_nested(value)
                })?)
            }
        }
        impl $crate::plan::copy::stored_plan::Field for $ty {
            $(fn fits(&self, $window: &mut $crate::plan::copy::stored_plan::Window) -> bool {
                let $source=self;
                $window.slots(1) && $fits
            })?
            async fn copy_from<'s>(
                &'s self,
                input: &mut $crate::plan::copy::stored_plan::Input<'s>,
            ) -> $crate::plan::copy::stored_plan::CaptureResult<Self> {
                let fields = self.submit_stored_projection(input.control(), &mut input.work)?;
                input.take_nested($crate::construction::receive(fields).await?)
            }
        }
    };
}
pub(crate) use projection;

macro_rules! project_struct {
    ($mode:ident, $ty:ty { $($field:ident),* $(,)? } $(skip { $($skip:ident),* $(,)? })?) => {
        impl $crate::plan::copy::stored_plan::Inline for $ty {
            fn project(&self, input: &mut $crate::plan::copy::stored_plan::Copying<'_>) -> $crate::plan::copy::stored_plan::CaptureResult<Self> {
                $crate::plan::copy::stored_plan::check(input)?;
                Ok(Self { $($field: $crate::plan::copy::stored_plan::Inline::project(&self.$field, input)?,)* $($($skip: Default::default(),)*)? })
            }
        }
        $crate::plan::copy::stored_plan::projection!($mode, $ty, source, input, Self {
            $($field: input.field(&source.$field).await?,)*
            $($($skip: Default::default(),)*)?
        });
    };
}
pub(crate) use project_struct;

macro_rules! project_enum {
    (inline, $ty:ty { $($tag:literal => $variant:ident),* $(,)? }) => {
        impl $crate::plan::copy::stored_plan::Inline for $ty {
            fn project(&self, _: &mut $crate::plan::copy::stored_plan::Copying<'_>) -> $crate::plan::copy::stored_plan::CaptureResult<Self> {
                Ok(match self { $(Self::$variant => Self::$variant,)* })
            }
        }
        impl $crate::plan::copy::stored_plan::Field for $ty {
            async fn copy_from<'s>(&'s self, _: &mut $crate::plan::copy::stored_plan::Input<'s>) -> $crate::plan::copy::stored_plan::CaptureResult<Self> {
                Ok(match self { $(Self::$variant => Self::$variant,)* })
            }
        }
    };
    ($mode:ident, $ty:ty { $($tag:literal => $variant:ident $(($item:ident))? $({ $($field:ident),* $(,)? $(; $($skip:ident),+)? })?),* $(,)? } $(reject { $($bad:pat => $message:literal),* $(,)? })?) => {
        impl $crate::plan::copy::stored_plan::Inline for $ty {
            fn project(&self, input: &mut $crate::plan::copy::stored_plan::Copying<'_>) -> $crate::plan::copy::stored_plan::CaptureResult<Self> {
                $crate::plan::copy::stored_plan::check(input)?;
                Ok(match self {
                    $(Self::$variant $(($item))? $({$($field,)* $($($skip: _,)*)?})? => Self::$variant
                        $(($crate::plan::copy::stored_plan::Inline::project($item, input)?))?
                        $({$($field: $crate::plan::copy::stored_plan::Inline::project($field, input)?,)* $($($skip: Default::default(),)*)?})?,)*
                    $($($bad => return Err($crate::SQLError::Internal($message.into())),)*)?
                })
            }
        }
        $crate::plan::copy::stored_plan::projection!($mode, $ty, source, input, match source {
            $(Self::$variant $(($item))? $({ $($field,)* $($($skip: _,)*)? })? =>
                Self::$variant $((input.field($item).await?))? $({ $($field:input.field($field).await?,)* $($($skip:Default::default(),)*)? })?,)*
            $($($bad => return Err($crate::SQLError::Internal($message.into())),)*)?
        });
    };
}
pub(crate) use project_enum;

/// The caller owns the original plan and its receipt until all independently funded output fields are complete.
pub(crate) async fn capture<'s>(
    plan: &'s crate::plan::UnifiedPlan,
    control: ProductionControl<'s>,
    work: construction::Work<'s>,
) -> CaptureResult<Captured<crate::plan::UnifiedPlan>> {
    let mut input = Input::new(control, work);
    let plan = match plan {
        crate::plan::UnifiedPlan::Query(query) => {
            crate::plan::UnifiedPlan::Query(input.field(query).await?)
        }
        crate::plan::UnifiedPlan::Command(command) => {
            crate::plan::UnifiedPlan::Command(input.field(command).await?)
        }
    };
    input.finish(plan)
}

/// Capture the original selected positions while retaining each written literal and nested query occurrence.
pub(crate) trait BindingSyntax: Sync {
    type Output: Send;
    fn fields<'s>(
        &'s self,
        input: &mut Input<'s>,
    ) -> impl std::future::Future<Output = CaptureResult<Self::Output>> + Send;
    async fn capture_syntax_before_binding<W: crate::catalog::work::Work + Send>(
        &self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> CaptureResult<Produced<Self::Output>> {
        control.check()?;
        let mut driver = construction::Driver::new(*control)?;
        let output = driver.work().submit(move |work| async move {
            let mut input = Input::new(*control, work);
            let syntax = self.fields(&mut input).await?;
            input.finish(syntax).map(|(syntax, _)| syntax)
        })?;
        while driver.advance().await? {
            work.checkpoint().await?;
        }
        control.check()?;
        construction::receive(output).await
    }
}

impl BindingSyntax for crate::plan::MutableExpressionPlan {
    type Output = crate::plan::ExpressionPlan;
    async fn fields<'s>(&'s self, input: &mut Input<'s>) -> CaptureResult<Self::Output> {
        Ok(crate::plan::ExpressionPlan {
            scalar: input.field(self.scalar()).await?,
            subqueries: input.list(self.queries()).await?,
        })
    }
}

impl BindingSyntax for crate::plan::QueryPlan {
    type Output = Self;
    async fn fields<'s>(&'s self, input: &mut Input<'s>) -> CaptureResult<Self> {
        input.field(self).await
    }
}

#[cfg(test)]
mod tests;
