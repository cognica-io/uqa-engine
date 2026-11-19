//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fields, variant, Node, Part, SQLError};
use crate::{
    ast::{
        CteMaterialization, FrameExclusion, FrameMode, JoinKind, JoinUsing, LockStrength, LockWait,
        LockingClause, NullsOrder, OperatorJoinRelations, OverridingKind, ReturningAliases,
        SetOpKind, WindowDefinition,
    },
    catalog::roles::RoleReference,
    ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};

image_struct!(OperatorJoinRelations { left, right });
image_struct!(JoinUsing { columns, alias });
image_struct!(LockingClause {
    strength,
    wait,
    relations
});
image_struct!(ReturningAliases {
    old,
    new,
    old_explicit,
    new_explicit
});
image_struct!(WindowDefinition<ScalarWindowSpec> { name, inherited, spec });
image_struct!(ScalarWindowSpec {
    definition,
    partition_by,
    order_by,
    frame
});
image_struct!(ScalarOrder {
    expr,
    descending,
    nulls
});
image_struct!(ScalarWindowFrame {
    mode,
    start,
    end,
    between,
    exclusion
});
image_enum!(ScalarFrameBound {
    0 => UnboundedPreceding, 1 => UnboundedFollowing, 2 => CurrentRow,
    3 => Preceding(value), 4 => Following(value),
});
image_enum!(NullsOrder { 0 => First, 1 => Last });
image_enum!(SetOpKind { 0 => Union, 1 => Intersect, 2 => Except });
image_enum!(CteMaterialization { 0 => Default, 1 => Materialized, 2 => NotMaterialized });
image_enum!(JoinKind { 0 => Inner, 1 => Left, 2 => Right, 3 => Full, 4 => Cross });
image_enum!(FrameMode { 0 => Rows, 1 => Range, 2 => Groups });
image_enum!(FrameExclusion { 0 => NoOthers, 1 => CurrentRow, 2 => Group, 3 => Ties });
image_enum!(LockStrength { 0 => ForKeyShare, 1 => ForShare, 2 => ForNoKeyUpdate, 3 => ForUpdate });
image_enum!(LockWait { 0 => Block, 1 => SkipLocked, 2 => NoWait });
image_enum!(OverridingKind { 0 => SystemValue, 1 => UserValue });

impl Node for crate::ColumnIdentity {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match position {
            0 => Some(Part::OptionalText(self.qualifier())),
            1 => Some(Part::Text(self.column())),
            _ => None,
        })
    }
}

impl Node for RoleReference {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match self {
            Self::Named(name) => variant(position, 0, &[name]),
            Self::Bound(binding) => variant(
                position,
                1,
                &[&binding.name, &binding.oid, &binding.object_id],
            ),
        })
    }
}
