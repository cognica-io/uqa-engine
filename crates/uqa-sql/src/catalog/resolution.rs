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

/// Complete outcome of resolving one relation reference through a statement namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelationResolution {
    Found(String, &'static str),
    MissingRelation,
    MissingSchema(String),
}

impl RelationResolution {
    /// Collapse namespace absence only for SQL boundaries whose contract reports an undefined relation for either absence outcome.
    pub fn into_found(self) -> Option<(String, &'static str)> {
        match self {
            Self::Found(name, kind) => Some((name, kind)),
            Self::MissingRelation | Self::MissingSchema(_) => None,
        }
    }
}

/// Bind rename-source diagnostics without losing the distinction between missing schemas and relations.
pub fn resolve_relation_rename_source(
    resolution: RelationResolution,
    name: &str,
    if_exists: bool,
    notice: &mut dyn FnMut(&str),
) -> Result<Option<(String, &'static str)>, crate::SQLError> {
    match resolution {
        RelationResolution::Found(canonical, kind) => Ok(Some((canonical, kind))),
        RelationResolution::MissingSchema(_) | RelationResolution::MissingRelation if if_exists => {
            let (_, local_name) = uqa_core::RelationIdentity::parse_reference(name)
                .map_err(crate::SQLError::Internal)?;
            notice(&format!(
                "relation \"{local_name}\" does not exist, skipping"
            ));
            Ok(None)
        }
        RelationResolution::MissingSchema(schema) => Err(crate::SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        }),
        RelationResolution::MissingRelation => Err(crate::SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!("relation \"{name}\" does not exist"),
        }),
    }
}
