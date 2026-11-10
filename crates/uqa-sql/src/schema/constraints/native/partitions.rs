//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compare derived foreign-key rows with the complete captured partition membership.

use super::{bound_relation_identity_with_control, invalid, Catalog, SQLError};
use crate::schema::inheritance::foreign_keys::foreign_key_families;
use uqa_core::memory::{BudgetedMap, ProductionControl, ProductionMap};

#[derive(Clone, Copy)]
struct Link {
    parent: [u8; 16],
    next_sibling: Option<[u8; 16]>,
}

pub(super) struct Partitions<'a> {
    links: ProductionMap<'a, [u8; 16], Link>,
    first_child: ProductionMap<'a, [u8; 16], [u8; 16]>,
    families: ProductionMap<'a, ([u8; 16], [u8; 16]), ()>,
    control: ProductionControl<'a>,
}

impl<'a> Partitions<'a> {
    pub(super) fn new(catalog: &Catalog<'a>) -> Result<Self, SQLError> {
        let mut result = Self {
            links: ProductionMap::new(catalog.control),
            first_child: ProductionMap::new(catalog.control),
            families: ProductionMap::new(catalog.control),
            control: catalog.control,
        };
        for (_, table) in &catalog.tables {
            result.control.check()?;
            for family in foreign_key_families(table.columns, table.foreign_keys) {
                result.families.insert_copy((table.object_id, family), ())?;
            }
            let Some(hierarchy) = table.hierarchy.filter(|hierarchy| hierarchy.is_partition())
            else {
                continue;
            };
            if hierarchy.parents.len() != 1 {
                return Err(invalid("partition parent cardinality"));
            }
            let name =
                bound_relation_identity_with_control(&hierarchy.parents[0], &result.control)?;
            let parent = catalog
                .tables
                .get(&*name)
                .ok_or_else(|| invalid("missing partition parent"))?;
            if parent.object_id == table.object_id
                || parent
                    .hierarchy
                    .is_none_or(|hierarchy| hierarchy.partition_spec.is_none())
            {
                return Err(invalid("partition parent incarnation or kind"));
            }
            let previous = result
                .first_child
                .insert_copy(parent.object_id, table.object_id)?;
            result.links.insert_copy(
                table.object_id,
                Link {
                    parent: parent.object_id,
                    next_sibling: previous,
                },
            )?;
        }
        Ok(result)
    }

    pub(super) fn parent(&self, table: &[u8; 16]) -> Option<[u8; 16]> {
        self.links.get(table).map(|link| link.parent)
    }

    pub(super) fn inherits_family(&self, table: &[u8; 16], family: Option<[u8; 16]>) -> bool {
        self.parent(table)
            .zip(family)
            .is_some_and(|(parent, family)| self.families.get(&(parent, family)).is_some())
    }

    pub(super) fn complete(
        &self,
        target: [u8; 16],
        seen: &BudgetedMap<[u8; 16], ()>,
    ) -> Result<(), SQLError> {
        for parent in std::iter::once(target).chain(seen.iter().map(|(table, ())| *table)) {
            self.control.check()?;
            let mut next = self.first_child.get(&parent).copied();
            while let Some(child) = next {
                self.control.check()?;
                if !seen.contains_key(&child) {
                    return Err(invalid("missing derived referenced-partition constraint"));
                }
                next = self
                    .links
                    .get(&child)
                    .expect("indexed partition child")
                    .next_sibling;
            }
        }
        Ok(())
    }
}
