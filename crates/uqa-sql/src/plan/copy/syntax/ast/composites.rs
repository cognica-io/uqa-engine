//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::CompositeAttributeAddition {
    attribute,
    declaration
});

syntax_struct!(crate::ast::CompositeAttributeDefinition {
    name,
    ty,
    collation,
    setof
});

syntax_struct!(crate::ast::CompositeAttributeRemoval {
    name,
    if_exists,
    cascade
});

syntax_struct!(crate::ast::CompositeRowBinding {
    ty,
    attributes,
    layout,
    argument_types
});

syntax_struct!(crate::ast::CompositeRowLayout { width, implicit });

syntax_struct!(crate::ast::CreateCompositeType { name, attributes });
