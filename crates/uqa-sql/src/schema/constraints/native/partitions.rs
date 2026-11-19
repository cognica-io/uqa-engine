//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compare derived foreign-key rows with the complete captured partition membership.

use super::{
    bound_relation_identity_with_work, invalid, retire_entries, BudgetedMap, Catalog, CatalogWork,
    ReferenceWork, SQLError,
};
use crate::schema::inheritance::foreign_keys::attachment::{declaration_slots, Attachment};

#[derive(Clone, Copy)]
struct Link {
    parent: [u8; 16],
    next_sibling: Option<[u8; 16]>,
}

pub(super) struct Partitions<'a> {
    links: BudgetedMap<[u8; 16], Link>,
    first_child: BudgetedMap<[u8; 16], [u8; 16]>,
    families: BudgetedMap<([u8; 16], [u8; 16]), Attachment<'a>>,
}

impl<'a> Partitions<'a> {
    pub(super) async fn new(
        catalog: &Catalog<'a>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<Self, SQLError> {
        let mut result = Self {
            links: BudgetedMap::new(catalog.memory),
            first_child: BudgetedMap::new(catalog.memory),
            families: BudgetedMap::new(catalog.memory),
        };
        for (_, table) in &catalog.tables {
            input.checkpoint().await?;
            for slot in declaration_slots(table.columns, table.foreign_keys) {
                input.checkpoint().await?;
                let Some((family, definition)) = slot else {
                    continue;
                };
                if result
                    .families
                    .insert((table.object_id, family), definition)?
                    .is_some()
                {
                    return Err(invalid("duplicate foreign-key family on one relation"));
                }
            }
            let Some(hierarchy) = table.hierarchy.filter(|hierarchy| hierarchy.is_partition())
            else {
                continue;
            };
            if hierarchy.parents.len() != 1 {
                return Err(invalid("partition parent cardinality"));
            }
            let name =
                bound_relation_identity_with_work(&hierarchy.parents[0], input.control, input.work)
                    .await?;
            let parent = catalog
                .tables
                .get_from(&*name, input)
                .await?
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
                .insert(parent.object_id, table.object_id)?;
            result.links.insert(
                table.object_id,
                Link {
                    parent: parent.object_id,
                    next_sibling: previous,
                },
            )?;
        }
        result.validate_families(catalog.memory, input).await?;
        Ok(result)
    }

    async fn validate_families(
        &self,
        memory: &super::MemoryBudget,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        let mut roots = BudgetedMap::new(memory);
        for ((table, family), definition) in &self.families {
            input.checkpoint().await?;
            if !self.inherits_family(table, Some(*family))
                && roots.insert(*family, *table)?.is_some()
            {
                return Err(invalid(
                    "foreign-key family has independent declaring owners",
                ));
            }
            let mut next = self.first_child.get(table).copied();
            while let Some(child) = next {
                input.checkpoint().await?;
                let inherited = self
                    .families
                    .get(&(child, *family))
                    .ok_or_else(|| invalid("missing inherited foreign-key family"))?;
                if !inherited
                    .same_reference_with_work(*definition, input)
                    .await?
                    || !inherited.same_options(*definition)
                {
                    return Err(invalid("inherited foreign-key definition"));
                }
                next = self
                    .links
                    .get(&child)
                    .expect("indexed partition child")
                    .next_sibling;
            }
        }
        retire_entries(roots.into_entries(), input.control, input.work).await
    }

    pub(super) fn parent(&self, table: &[u8; 16]) -> Option<[u8; 16]> {
        self.links.get(table).map(|link| link.parent)
    }

    pub(super) fn inherits_family(&self, table: &[u8; 16], family: Option<[u8; 16]>) -> bool {
        self.parent(table)
            .zip(family)
            .is_some_and(|(parent, family)| self.families.get(&(parent, family)).is_some())
    }

    pub(super) async fn complete(
        &self,
        target: [u8; 16],
        seen: &BudgetedMap<[u8; 16], ()>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        for parent in std::iter::once(target).chain(seen.iter().map(|(table, ())| *table)) {
            input.checkpoint().await?;
            let mut next = self.first_child.get(&parent).copied();
            while let Some(child) = next {
                input.checkpoint().await?;
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
    pub(super) async fn retire(
        self,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        retire_entries(self.families.into_entries(), input.control, input.work).await?;
        retire_entries(self.first_child.into_entries(), input.control, input.work).await?;
        retire_entries(self.links.into_entries(), input.control, input.work).await
    }
}
