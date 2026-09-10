//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation namespace inputs for static SQL analysis.

use crate::SQLError;

/// Immutable session inputs used to resolve unqualified relation names during one statement.
#[derive(Clone)]
pub struct RelationNameResolution {
    pub search_path: Vec<String>,
    pub temporary_schema: String,
    pub temporary_namespace_allocated: bool,
    pub current_user: String,
    pub lookup_mode: RelationLookupMode,
}

/// Whether a query resolves session-visible names or follows catalog identities captured when a stored expression was defined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationLookupMode {
    Dynamic,
    Bound,
}

impl RelationNameResolution {
    pub fn search_path(&self) -> &[String] {
        &self.search_path
    }

    pub fn search_path_contains(&self, schema: &str) -> bool {
        self.search_path.iter().any(|candidate| candidate == schema)
    }

    pub fn current_user(&self) -> &str {
        &self.current_user
    }

    pub fn lookup_mode(&self) -> RelationLookupMode {
        self.lookup_mode
    }

    pub fn qualified_schema(&self, name: &str) -> Result<Option<(String, String)>, SQLError> {
        let (schema, _) = uqa_core::RelationIdentity::parse_reference(name).map_err(|error| {
            SQLError::Internal(format!("resolve catalog relation `{name}`: {error}"))
        })?;
        Ok(schema.map(|schema| {
            let resolved = if schema == "pg_temp" {
                self.temporary_schema.clone()
            } else {
                schema.clone()
            };
            (schema, resolved)
        }))
    }

    pub fn set_lookup_mode(&mut self, lookup_mode: RelationLookupMode) -> RelationLookupMode {
        std::mem::replace(&mut self.lookup_mode, lookup_mode)
    }

    pub fn raw_relation_lookup_candidates(
        &self,
        name: &str,
    ) -> Result<Vec<uqa_core::RelationIdentity>, SQLError> {
        let (schema, relation) =
            uqa_core::RelationIdentity::parse_reference(name).map_err(|error| {
                SQLError::Internal(format!("resolve catalog relation `{name}`: {error}"))
            })?;
        if let Some(schema) = schema {
            let schema = if schema == "pg_temp" {
                self.temporary_schema.clone()
            } else {
                schema
            };
            return Ok(vec![uqa_core::RelationIdentity::new(schema, relation)]);
        }
        let mut candidates = vec![uqa_core::RelationIdentity::new(
            &self.temporary_schema,
            &relation,
        )];
        candidates.extend(
            self.search_path
                .iter()
                .filter(|schema| *schema != "pg_catalog" && *schema != "information_schema")
                .map(|schema| uqa_core::RelationIdentity::new(schema, &relation)),
        );
        Ok(candidates)
    }
}
