//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authority copies retain complete tuple identities, bound endpoints and privilege bits.

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(
    crate::catalog::security::table_binding::BoundTableAclEntry {
        role,
        grantor,
        privileges,
        grant_options
    }
);
syntax_struct!(crate::catalog::security::system_relations::SystemAcl { revision, acl });
syntax_struct!(
    crate::catalog::security::system_relations::SystemRelationSecurity { table, columns }
);
syntax_struct!(uqa_core::catalog_acl::RelationAclRevisions { relation, columns });
syntax_struct!(crate::catalog::security::BoundTableSecurity {
    role_owner,
    acl,
    column_acls,
    acl_revisions
});
