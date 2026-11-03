//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete stored type definitions retain identity, dropped slots and bound expressions.

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(uqa_core::RelationIdentity { schema, name });
syntax_struct!(crate::catalog::domain::StoredDomain {
    object_id,
    oid,
    array_oid,
    identity,
    owner,
    definition,
    array_name,
    usage_acl
});
syntax_struct!(crate::catalog::enum_type::StoredEnum {
    object_id,
    oid,
    array_oid,
    array_name,
    identity,
    owner,
    labels,
    usage_acl
});
syntax_struct!(crate::catalog::enum_type::StoredEnumLabel {
    oid,
    key,
    label,
    sort_order
});
syntax_struct!(crate::catalog::composite_type::StoredComposite {
    object_id,
    oid,
    relation_oid,
    array_oid,
    array_name,
    identity,
    owner,
    attributes,
    usage_acl
});
syntax_struct!(crate::catalog::composite_type::StoredCompositeAttribute {
    name,
    ty,
    collation,
    collation_oid,
    number,
    dropped
});
syntax_struct!(crate::catalog::range_type::StoredRange {
    object_id,
    oid,
    array_oid,
    array_name,
    identity,
    owner,
    subtype,
    subtype_operator_class,
    collation_oid,
    canonical_oid,
    subtype_difference_oid,
    multirange,
    usage_acl
});
syntax_struct!(crate::catalog::range_type::StoredMultirange {
    object_id,
    oid,
    array_oid,
    array_name,
    identity
});
syntax_struct!(crate::catalog::shell_type::StoredShellType {
    object_id,
    oid,
    identity,
    owner,
    usage_acl
});

impl SyntaxCopy for uqa_core::EnumLabelKey {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        copying.check()?;
        copying.reserve(self.as_bytes().len())?;
        Ok(self.clone())
    }
}
