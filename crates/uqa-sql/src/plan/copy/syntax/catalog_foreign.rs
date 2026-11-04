//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::catalog::foreign_wrapper::ForeignWrapperReference { oid, object_id });
syntax_enum!(crate::catalog::foreign_wrapper::NativeForeignWrapper {
    Memory,
    DuckDB,
    Arrow
});
syntax_struct!(crate::catalog::foreign_wrapper::ForeignWrapperFunction { oid, binding });
syntax_enum!(crate::catalog::foreign_wrapper::ForeignWrapperHandler {
    None,
    Native(value),
    Function(value)
});
syntax_struct!(crate::catalog::foreign_wrapper::ForeignWrapperDefinition {
    name,
    identity,
    owner,
    handler,
    validator,
    options
});
syntax_struct!(crate::catalog::foreign_server::ForeignServerMetadata {
    oid,
    object_id,
    owner,
    server_type,
    version,
    option_order,
    wrapper_reference
});
