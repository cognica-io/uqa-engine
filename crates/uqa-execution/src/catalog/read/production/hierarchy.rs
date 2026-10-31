//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchy names borrow immutable table identities and admit selected names and traversal nodes.

use super::{CatalogReadView, RelationNameResolution, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionMap, ProductionVec},
    RelationIdentity,
};

impl CatalogReadView {
    pub fn hierarchy_scan_tables_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        include_descendants: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        let root = candidates
            .iter()
            .find(|relation| self.snapshot.tables.contains_key(*relation))
            .ok_or_else(|| match control.copy_text(name) {
                Ok(name) => SQLError::UnknownTable(name.into_parts().0),
                Err(error) => error.into(),
            })?;
        let mut output = ProductionVec::new(*control);
        if include_descendants {
            self.collect_hierarchy_descendants_with_control(
                root,
                &mut ProductionMap::new(*control),
                &mut ProductionMap::new(*control),
                &mut output,
                control,
            )?;
        } else {
            output.push_produced(root.qualified_name_with_control(control)?)?;
        }
        Ok(output.finish()?)
    }

    fn collect_hierarchy_descendants_with_control<'a>(
        &'a self,
        parent: &'a RelationIdentity,
        visiting: &mut ProductionMap<'_, &'a RelationIdentity, ()>,
        visited: &mut ProductionMap<'_, &'a RelationIdentity, ()>,
        output: &mut ProductionVec<'_, String>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if visiting.get(parent).is_some() {
            let name = parent.qualified_name_with_control(control)?;
            return Err(SQLError::Internal(
                control
                    .format(format_args!("table inheritance cycle reaches `{}`", &*name))?
                    .into_parts()
                    .0,
            ));
        }
        if visited.insert_copy(parent, ())?.is_some() {
            return Ok(());
        }
        visiting.insert_copy(parent, ())?;
        output.push_produced(parent.qualified_name_with_control(control)?)?;
        let name = parent.qualified_name_with_control(control)?;
        for (identity, table) in self.snapshot.tables.iter() {
            control.check()?;
            if table
                .hierarchy
                .parents
                .iter()
                .any(|candidate| candidate == &*name)
            {
                self.collect_hierarchy_descendants_with_control(
                    identity, visiting, visited, output, control,
                )?;
            }
        }
        visiting.remove(parent);
        Ok(())
    }
}
