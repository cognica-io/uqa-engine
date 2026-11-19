//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fields, variant, Node, Part, SQLError};
use crate::ast::{
    FrameBound, OrderBy, Projection, WindowCallModifiers, WindowDefinition, WindowFrame,
    WindowReference, WindowReferenceKind, WindowSpec,
};

image_struct!(Projection { expr, alias });
image_struct!(OrderBy {
    expr,
    descending,
    nulls
});
image_struct!(WindowDefinition {
    name,
    inherited,
    spec
});
image_struct!(WindowSpec { definition, reference, partition_by, order_by, frame } skip { raw_definition });
image_struct!(WindowReference { name, kind });
image_enum!(WindowReferenceKind { 0 => Direct, 1 => Copy });
image_struct!(WindowFrame {
    mode,
    start,
    end,
    between,
    exclusion
});
image_enum!(FrameBound {
    0 => UnboundedPreceding, 1 => UnboundedFollowing, 2 => CurrentRow,
    3 => Preceding(value), 4 => Following(value),
});
image_struct!(WindowCallModifiers {
    distinct,
    ordered,
    within_group
});
