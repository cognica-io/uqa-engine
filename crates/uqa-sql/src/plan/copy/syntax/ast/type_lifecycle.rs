//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::AlterTypeObject { kind, name, action });

syntax_enum!(crate::ast::AlterTypeObjectAction {
    RenameTo(value0),
    SetSchema(value0),
    OwnerTo(value0),
    RenameAttribute { from, to },
    AddAttributes(value0),
    AlterAttributes { removals, type_changes, additions }
});

syntax_struct!(crate::ast::CommentType {
    kind,
    name,
    description
});

syntax_enum!(crate::ast::TypeObjectKind { Type, Domain });
