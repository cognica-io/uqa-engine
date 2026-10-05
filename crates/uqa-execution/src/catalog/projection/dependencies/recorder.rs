//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_depend` rows in the order the catalog records them. Dependencies on pinned objects are recorded here and removed once every object that can be referenced is known, as `recordMultipleDependencies` never stores them.

use super::References;
use std::collections::BTreeSet;
use uqa_sql::catalog::dependencies::{Dependency, DependencyKind, ObjectAddress, RELATION_CLASS};

#[derive(Debug, Default)]
pub(super) struct DependencyRecorder {
    edges: Vec<Dependency>,
}

impl DependencyRecorder {
    /// `recordDependencyOn`.
    pub(super) fn record(
        &mut self,
        dependent: ObjectAddress,
        referenced: ObjectAddress,
        kind: DependencyKind,
    ) {
        self.edges.push(Dependency {
            dependent,
            referenced,
            kind,
        });
    }

    /// `record_object_address_dependencies` and `recordDependencyOnExpr`: every distinct reference, a relation's columns in place of the whole relation.
    pub(super) fn record_references(
        &mut self,
        dependent: ObjectAddress,
        references: References,
        kind: DependencyKind,
    ) {
        for referenced in references.deduplicated() {
            self.record(dependent, referenced, kind);
        }
    }

    /// `recordDependencyOnSingleRelExpr`: references to `relation` itself take `self_kind`, and with `reverse_self` the relation's columns depend on `dependent` instead.
    pub(super) fn record_single_relation(
        &mut self,
        dependent: ObjectAddress,
        references: References,
        relation: u32,
        (kind, self_kind): (DependencyKind, DependencyKind),
        reverse_self: bool,
    ) {
        if kind == self_kind && !reverse_self {
            self.record_references(dependent, references, kind);
            return;
        }
        let (own, external): (Vec<_>, Vec<_>) =
            references.deduplicated().into_iter().partition(|address| {
                address.class_id == RELATION_CLASS && address.object_id == relation
            });
        for address in own {
            if reverse_self {
                self.record(address, dependent, self_kind);
            } else {
                self.record(dependent, address, self_kind);
            }
        }
        for address in external {
            self.record(dependent, address, kind);
        }
    }

    /// The recorded rows whose referenced objects are not pinned.
    pub(super) fn finish(self, unpinned: &BTreeSet<(u32, u32)>) -> Vec<Dependency> {
        self.edges
            .into_iter()
            .filter(|edge| {
                unpinned.contains(&(edge.referenced.class_id, edge.referenced.object_id))
            })
            .collect()
    }
}
