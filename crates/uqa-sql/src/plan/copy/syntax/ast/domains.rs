//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::AlterDomain { name, action });

syntax_enum!(crate::ast::AlterDomainAction {
    AddCheck { constraint },
    AddNotNull { constraint },
    DropConstraint { name, if_exists, cascade },
    ValidateConstraint { name }
});

syntax_struct!(crate::ast::CreateDomain {
    name,
    base,
    collation,
    collation_oid,
    default,
    not_null,
    checks
});

syntax_struct!(crate::ast::DomainCheck {
    name,
    catalog_identity,
    expression,
    validated
});

syntax_struct!(crate::ast::DomainNotNull {
    name,
    catalog_identity
});
