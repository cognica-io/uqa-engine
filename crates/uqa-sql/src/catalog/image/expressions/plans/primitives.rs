//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Node, Part, SQLError};
use uqa_core::memory::OwnedSet;

impl<T: Node> Node for Vec<T> {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(if position == 0 {
            Some(Part::Count(self.len()))
        } else {
            self.get(position - 1).map(|value| Part::Node(value))
        })
    }
}

impl<T: Node> Node for Option<T> {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match position {
            0 => Some(Part::Tag(u8::from(self.is_some()))),
            1 => self.as_ref().map(|value| Part::Node(value)),
            _ => None,
        })
    }
}

impl<T: Node> Node for Box<T> {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok((position == 0).then_some(Part::Node(self.as_ref())))
    }
}

impl Node for String {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok((position == 0).then_some(Part::Text(self)))
    }
}

impl<const N: usize> Node for [u8; N] {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok((position == 0).then_some(Part::Bytes(self)))
    }
}

macro_rules! primitive {
    ($ty:ty, $value:ident => $part:expr) => {
        impl Node for $ty {
            fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
                let $value = self;
                Ok((position == 0).then_some($part))
            }
        }
    };
}

primitive!(bool, value => Part::Tag(u8::from(*value)));
primitive!(usize, value => Part::Count(*value));
primitive!(u32, value => Part::U32(*value));
primitive!(u64, value => Part::U64(*value));
primitive!(crate::ScalarExpr, value => Part::Scalar(value));
primitive!(crate::ColumnType, value => Part::Type(value));
primitive!(crate::ast::FunctionBinding, value => Part::Binding(value));
primitive!(crate::ast::InternalRelationId, value => Part::U64(value.raw()));

impl Node for OwnedSet<crate::ColumnIdentity> {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match position {
            0 => Some(Part::Count(self.len())),
            1 => Some(Part::Columns(self)),
            _ => None,
        })
    }
}

impl Node for OwnedSet<usize> {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match position {
            0 => Some(Part::Count(self.len())),
            1 => Some(Part::Positions(self)),
            _ => None,
        })
    }
}
