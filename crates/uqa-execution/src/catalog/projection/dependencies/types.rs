//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of user-defined types, as `GenerateTypeDependencies` records them for enums, domains and their array types, and `domainAddConstraint` for domain constraints.

use super::{ColumnScope, ConstraintOwner, DependencyBuilder, MemberObject, References};
use uqa_sql::catalog::dependencies::{DependencyKind, ObjectAddress, CONSTRAINT_CLASS, TYPE_CLASS};
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    pub(super) fn record_types(&mut self) -> Result<(), SQLError> {
        let catalog = self.catalog;
        for definition in catalog.enums() {
            // `EnumValuesCreate`: the array type, then the enum.
            self.record_array_type(definition.array_oid, definition.oid);
            let enum_type = ObjectAddress::whole(TYPE_CLASS, definition.oid);
            self.record_namespace(enum_type, &definition.identity.schema);
        }
        for domain in catalog.domains() {
            let domain_type = ObjectAddress::whole(TYPE_CLASS, domain.oid);
            let array = super::catalog_oid(uqa_sql::catalog::type_metadata::pg_domain_array_oid(
                domain.oid,
                domain.array_oid,
            ))?;
            self.record_array_type(array, domain.oid);
            // The domain's namespace and base type, then its default.
            self.record_namespace(domain_type, &domain.identity.schema);
            let mut references = References::default();
            if let Ok(base) = u32::try_from(uqa_sql::catalog::type_metadata::pg_type_oid(
                &domain.definition.base,
            )) {
                references.add_type(base);
            }
            self.recorder
                .record_references(domain_type, references, DependencyKind::Normal);
            if let Some(default) = &domain.definition.default {
                // `DefineDomain` coerces the default to the base type.
                let mut references = References::default();
                self.expressions().collect_assigned(
                    default,
                    &domain.definition.base,
                    (ColumnScope::None, &[]),
                    &mut references,
                )?;
                self.recorder
                    .record_references(domain_type, references, DependencyKind::Normal);
            }
            let not_null = domain.definition.not_null.iter().map(|constraint| {
                (
                    constraint.name.as_deref(),
                    constraint.catalog_identity,
                    None,
                )
            });
            let checks = domain.definition.checks.iter().map(|constraint| {
                (
                    constraint.name.as_deref(),
                    constraint.catalog_identity,
                    Some(&constraint.expression),
                )
            });
            for (name, identity, expression) in not_null.chain(checks).collect::<Vec<_>>() {
                let (Some(name), Some(identity)) = (name, identity) else {
                    return Err(SQLError::Internal(format!(
                        "constraint of domain {} has no catalog identity",
                        domain.identity.qualified_name()
                    )));
                };
                let oid = super::catalog_oid(identity.oid)?;
                self.objects.add_member(
                    CONSTRAINT_CLASS,
                    oid,
                    MemberObject::Constraint {
                        name: name.to_string(),
                        owner: ConstraintOwner::Domain(domain.oid),
                        not_null: expression.is_none(),
                    },
                );
                let constraint = ObjectAddress::whole(CONSTRAINT_CLASS, oid);
                self.recorder
                    .record(constraint, domain_type, DependencyKind::Auto);
                if let Some(expression) = expression {
                    let mut references = References::default();
                    self.expressions()
                        .collect(expression, ColumnScope::None, &mut references)?;
                    self.recorder
                        .record_references(constraint, references, DependencyKind::Normal);
                }
            }
        }
        Ok(())
    }

    /// An implicitly created array type is part of its element type.
    fn record_array_type(&mut self, array: u32, element: u32) {
        self.recorder.record(
            ObjectAddress::whole(TYPE_CLASS, array),
            ObjectAddress::whole(TYPE_CLASS, element),
            DependencyKind::Internal,
        );
    }
}
