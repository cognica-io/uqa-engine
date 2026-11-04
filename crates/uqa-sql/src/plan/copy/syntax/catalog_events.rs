//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::catalog::events::StoredTrigger {
    definition,
    function_object_id,
    enabled,
    object_id,
    constraint_name,
    catalog_oid,
    constraint_catalog_oid
});

syntax_struct!(crate::catalog::events::RuleColumnDependency { relation, column });
syntax_struct!(crate::catalog::events::RuleRoutineDependency {
    object_id,
    name,
    argument_types
});
