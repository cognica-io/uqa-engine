//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete syntax copies reuse allocation-owning constructors; field lists and enum matches remain exhaustive.

use super::{Copying, Result};
use uqa_core::memory::{OwnedMap, OwnedSet};

pub(crate) trait SyntaxCopy: Sized {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self>;
}

macro_rules! syntax_struct {
    ($ty:ty { $($field:ident),* $(,)? }) => {
        impl SyntaxCopy for $ty {
            fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
                copying.check()?;
                Ok(Self { $($field: copying.field(stringify!($field), |copying| self.$field.copy_syntax(copying))?),* })
            }
        }
    };
}

macro_rules! syntax_enum {
    ($ty:ty { $($variant:ident $(($($item:ident),+))? $({$($field:ident),*})?),* $(,)? }) => {
        impl SyntaxCopy for $ty {
            fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
                copying.check()?;
                Ok(match self {
                    $(Self::$variant $(($($item),+))? $({$($field),*})? =>
                        Self::$variant $(($(copying.field(stringify!($item), |copying| $item.copy_syntax(copying))?),+))? $({$($field: copying.field(stringify!($field), |copying| $field.copy_syntax(copying))?),*})?),*
                })
            }
        }
    };
}

macro_rules! copy_inline {
    ($($ty:ty),+ $(,)?) => {$ (
        impl SyntaxCopy for $ty {
            fn copy_syntax(&self, _: &mut Copying<'_>) -> Result<Self> { Ok(*self) }
        }
    )+};
}

copy_inline!(
    bool,
    usize,
    u8,
    u16,
    u32,
    u64,
    i8,
    i16,
    i32,
    i64,
    f32,
    f64,
    crate::ast::InternalColumnRef,
    crate::ast::InternalRelationId,
    crate::ast::ArraySliceBounds,
    crate::ast::FunctionCallSyntax,
    crate::catalog::collations::CollationSelection,
    uqa_core::catalog_identity::CatalogObjectIdentity,
    uqa_core::catalog_role::RoleIdentity,
    std::num::NonZeroU32
);

impl<T: Copy, const N: usize> SyntaxCopy for [T; N] {
    fn copy_syntax(&self, _: &mut Copying<'_>) -> Result<Self> {
        Ok(*self)
    }
}
impl SyntaxCopy for String {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.text(self)
    }
}
impl<T: SyntaxCopy> SyntaxCopy for Option<T> {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        self.as_ref()
            .map(|value| value.copy_syntax(copying))
            .transpose()
    }
}
impl<T: SyntaxCopy> SyntaxCopy for Vec<T> {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.map(self.iter(), |copying, value| value.copy_syntax(copying))
    }
}
impl<T: SyntaxCopy> SyntaxCopy for Box<T> {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.boxed(|copying| (**self).copy_syntax(copying))
    }
}
impl<A: SyntaxCopy, B: SyntaxCopy> SyntaxCopy for (A, B) {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        Ok((self.0.copy_syntax(copying)?, self.1.copy_syntax(copying)?))
    }
}
impl<T: SyntaxCopy + Ord> SyntaxCopy for OwnedSet<T> {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.set(self, |copying, value| value.copy_syntax(copying))
    }
}
impl<K: SyntaxCopy + Ord, V: SyntaxCopy> SyntaxCopy for OwnedMap<K, V> {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        let mut output = Self::new();
        for (key, value) in self {
            copying.check()?;
            copying.memory.grow(Self::entry_bytes())?;
            output.insert(key.copy_syntax(copying)?, value.copy_syntax(copying)?);
        }
        Ok(output)
    }
}
impl<T> SyntaxCopy for std::sync::Arc<T> {
    fn copy_syntax(&self, _: &mut Copying<'_>) -> Result<Self> {
        Ok(Self::clone(self))
    }
}
impl SyntaxCopy for uqa_core::Value {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        let value = copying.control.copy_value(self)?;
        Ok(copying.take(value))
    }
}

impl SyntaxCopy for crate::plan::UnifiedPlan {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        Ok(match self {
            Self::Query(query) => Self::Query(copying.boxed(|copying| copying.query(query))?),
            Self::Command(command) => Self::Command(copying.boxed(|copying| {
                copying.field("command", |copying| command.as_ref().copy_syntax(copying))
            })?),
        })
    }
}
impl SyntaxCopy for crate::plan::QueryPlan {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.query(self)
    }
}
impl SyntaxCopy for crate::plan::ExpressionPlan {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.expression(self)
    }
}

macro_rules! owned_copy {
    ($($ty:ty => $method:ident),+ $(,)?) => {$ (
        impl SyntaxCopy for $ty {
            fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
                let value = self.$method(&copying.control)?;
                Ok(copying.take(value))
            }
        }
    )+};
}
owned_copy!(crate::ColumnType => clone_with_control,
    crate::ast::EnumTypeReference => clone_with_control,
    crate::ast::CompositeTypeReference => clone_with_control,
    crate::ast::RangeTypeReference => clone_with_control,
    crate::params::SQLTensor => copy_with_control,
    crate::ast::RoutineBodyOrigin => clone_with_control,
    crate::expr::composites::CompositeTypeDescriptor => clone_with_control);

impl SyntaxCopy for crate::ast::ReferenceName {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        Ok(Self::from(copying.text(self.as_str())?).with_position(self.position()))
    }
}

macro_rules! share_input {
    ($($ty:ty),+ $(,)?) => {$ (
        impl SyntaxCopy for $ty {
            fn copy_syntax(&self, _: &mut Copying<'_>) -> Result<Self> { Ok(self.clone()) }
        }
    )+};
}
// These immutable descriptors already retain their payload owners; cloning only shares those handles.
share_input!(
    crate::ast::RetainedRowSource,
    crate::ast::RetainedInputLiteral,
    crate::ast::WindowDefinitionSyntax,
    crate::RowSchema
);

syntax_enum!(crate::SQLParam {
    Scalar(value), TypedScalar { value, ty }, DeferredError { ty, error },
    Vector(values), Tensor(values), TypedTensor(tensor)
});

mod ast;
mod command;

syntax_struct!(crate::ast::ObjectAclEntry {
    role,
    grantor,
    grant_option
});
syntax_struct!(crate::expr::composites::CompositeConstantSource { value, descriptors });
syntax_struct!(crate::explain::ExplainOptions { options });
syntax_struct!(crate::explain::ExplainOption { name, value });
syntax_enum!(crate::explain::ExplainOptionValue { Boolean(value), Integer(value), Float(value), String(value) });
