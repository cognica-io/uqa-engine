//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pure node predicates consume one borrowed vertex without provider reentry.

use super::{CypherError, CypherExecutor, CypherExpr, GraphStore, NodePattern, Vertex, VertexId};

impl<G: GraphStore> CypherExecutor<'_, G> {
    pub(super) fn for_each_node_vertex(
        &self,
        pattern: &NodePattern,
        ids: &[VertexId],
        mut visit: impl FnMut(&Vertex) -> Result<(), CypherError>,
    ) -> Result<(), CypherError> {
        // These expression forms only inspect parameters and already-bound
        // values. Functions and nested patterns may access graph storage.
        let can_borrow = pattern.properties.as_ref().is_none_or(|properties| {
            properties.values().all(|expression| {
                matches!(
                    expression,
                    CypherExpr::Literal(_)
                        | CypherExpr::Parameter(_)
                        | CypherExpr::Variable(_)
                        | CypherExpr::PropertyAccess(_)
                )
            })
        });
        if can_borrow {
            let mut count = 0;
            let mut failure = None;
            let result = self.store.for_each_vertex_borrowed(ids, &mut |id, vertex| {
                if failure.is_some() {
                    return false;
                }
                if ids.get(count) != Some(&id)
                    || vertex.is_some_and(|vertex| vertex.vertex_id != id)
                {
                    failure = Some(CypherError::Storage(
                        "borrowed vertex identities differ from the requested order".into(),
                    ));
                    return false;
                }
                count += 1;
                if let Some(vertex) = vertex {
                    if let Err(error) = visit(vertex) {
                        failure = Some(error);
                        return false;
                    }
                }
                true
            });
            // Predicate errors precede provider cleanup and cancellation.
            if let Some(error) = failure {
                return Err(error);
            }
            match result? {
                Some(reported) if reported == count && count == ids.len() => return Ok(()),
                None if count == 0 => {}
                _ => {
                    return Err(CypherError::Storage(
                        "borrowed vertex visit count differs from the requested identities".into(),
                    ))
                }
            }
        }
        for id in ids {
            if let Some(vertex) = self.store.get_vertex(*id)? {
                visit(&vertex)?;
            }
        }
        Ok(())
    }
}
