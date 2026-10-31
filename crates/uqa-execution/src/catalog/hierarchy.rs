//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchy names borrow immutable table identities and admit selected names and traversal nodes.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionMap, ProductionVec},
    RelationIdentity,
};
use uqa_sql::SQLError;

/// Traverse borrowed catalog identities in stable registry order. The adapter lends only table state; traversal, admission and cycle diagnostics belong to Execution.
pub fn hierarchy_scan_entries_with_control<'a, T: 'a, I, F, C>(
    root: &'a RelationIdentity,
    include_descendants: bool,
    entries: F,
    is_child: C,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError>
where
    F: Fn() -> I,
    I: Iterator<Item = (&'a RelationIdentity, &'a T)>,
    C: Fn(&T, &str) -> bool,
{
    let mut traversal = HierarchyTraversal {
        entries,
        is_child,
        visiting: ProductionMap::new(*control),
        visited: ProductionMap::new(*control),
        output: ProductionVec::new(*control),
        control: *control,
    };
    if include_descendants {
        traversal.visit(root)?;
    } else {
        traversal
            .output
            .push_produced(root.qualified_name_with_control(control)?)?;
    }
    Ok(traversal.output.finish()?)
}

struct HierarchyTraversal<'a, 'c, F, C> {
    entries: F,
    is_child: C,
    visiting: ProductionMap<'c, &'a RelationIdentity, ()>,
    visited: ProductionMap<'c, &'a RelationIdentity, ()>,
    output: ProductionVec<'c, String>,
    control: ProductionControl<'c>,
}
impl<'a, T: 'a, I, F, C> HierarchyTraversal<'a, '_, F, C>
where
    F: Fn() -> I,
    I: Iterator<Item = (&'a RelationIdentity, &'a T)>,
    C: Fn(&T, &str) -> bool,
{
    fn visit(&mut self, parent: &'a RelationIdentity) -> Result<(), SQLError> {
        self.control.check()?;
        if self.visiting.get(parent).is_some() {
            let name = parent.qualified_name_with_control(&self.control)?;
            return Err(SQLError::Internal(
                self.control
                    .format(format_args!("table inheritance cycle reaches `{}`", &*name))?
                    .into_parts()
                    .0,
            ));
        }
        if self.visited.insert_copy(parent, ())?.is_some() {
            return Ok(());
        }
        self.visiting.insert_copy(parent, ())?;
        self.output
            .push_produced(parent.qualified_name_with_control(&self.control)?)?;
        let name = parent.qualified_name_with_control(&self.control)?;
        for (identity, table) in (self.entries)() {
            self.control.check()?;
            if (self.is_child)(table, &name) {
                self.visit(identity)?;
            }
        }
        self.visiting.remove(parent);
        Ok(())
    }
}
