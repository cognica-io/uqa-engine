//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation namespace inputs for static SQL analysis.

use crate::catalog::roles::RoleReference;
use crate::SQLError;

mod production;

/// The usable namespace order and creation target of one analysis. An explicitly placed `pg_catalog` can leave the lookup order unchanged while changing the creation namespace, which `SearchPathMatchesCurrentEnvironment` also compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveSearchPath {
    pub schemas: Vec<String>,
    pub creation_namespace: Option<String>,
}

/// Immutable session inputs used to resolve unqualified relation names during one statement.
#[derive(Clone)]
pub struct RelationNameResolution {
    pub search_path: Vec<String>,
    pub temporary_schema: String,
    pub temporary_namespace_allocated: bool,
    pub current_user: RoleReference,
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

    pub fn current_user(&self) -> &RoleReference {
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
        Ok(candidates::unqualified_candidates(
            &self.temporary_schema,
            &self.search_path,
            &relation,
        ))
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

pub fn missing_relation_notice(name: &str) -> Result<String, crate::SQLError> {
    let (_, local_name) =
        uqa_core::RelationIdentity::parse_reference(name).map_err(crate::SQLError::Internal)?;
    Ok(format!(
        "relation \"{local_name}\" does not exist, skipping"
    ))
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
            notice(&missing_relation_notice(name)?);
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

pub mod candidates;

pub mod creation;

/// The name `PostgreSQL`'s object descriptions give `relation`: its own name, quoted as an identifier needs, when the search path finds that relation by it, and its schema-qualified name otherwise.
pub fn described_relation_name(
    relation: &uqa_core::RelationIdentity,
    resolve: impl FnOnce(&str) -> Result<RelationResolution, SQLError>,
) -> Result<String, SQLError> {
    let local = crate::expr::quote_ident(&relation.name);
    let visible = match resolve(&local)? {
        RelationResolution::Found(found, _) => uqa_core::RelationIdentity::from_legacy_name(&found)
            .is_ok_and(|found| found == *relation),
        RelationResolution::MissingRelation | RelationResolution::MissingSchema(_) => false,
    };
    Ok(if visible {
        local
    } else {
        format!("{}.{local}", crate::expr::quote_ident(&relation.schema))
    })
}

/// Decode a relation identity already selected by binding. A stored identity must name its schema and must never resolve through the current search path or temporary-schema alias.
pub fn bound_relation_identity(name: &str) -> Result<uqa_core::RelationIdentity, SQLError> {
    Ok(bound_relation_identity_with_control(
        name,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary bound identity"))
}

/// Decode an already bound relation without consulting a search path. Exact original names and diagnostic buffers use the supplied allowance.
pub fn bound_relation_identity_with_control(
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<uqa_core::RelationIdentity>, SQLError> {
    super::work::inline(bound_relation_identity_with_work(name, control, &mut ()))
}

/// Decode an already selected relation identity and its original diagnostic under the caller's work window.
pub async fn bound_relation_identity_with_work<W: super::work::Work + Send>(
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<uqa_core::memory::Produced<uqa_core::RelationIdentity>, SQLError> {
    use super::work::{text_parts, ReferenceWork};
    use uqa_core::RelationReferenceInputError;
    control.check()?;
    let reference = match uqa_core::RelationIdentity::try_borrowed_reference_from(
        name,
        &mut ReferenceWork { control, work },
    )
    .await
    {
        Ok(reference) => reference,
        Err(RelationReferenceInputError::Input(error)) => return Err(error),
        Err(RelationReferenceInputError::Reference(error)) => {
            let [prefix, value, suffix] = error.message_parts();
            let message = text_parts(
                work,
                &[
                    "decode bound relation `",
                    name,
                    "`: ",
                    prefix,
                    value,
                    suffix,
                ],
                control,
            )
            .await?;
            return Err(SQLError::admit_internal_message(message, control)?);
        }
    };
    let Some(schema) = reference.schema else {
        let message = text_parts(
            work,
            &[
                "bound query contains non-canonical relation reference `",
                name,
                "`",
            ],
            control,
        )
        .await?;
        return Err(SQLError::admit_internal_message(message, control)?);
    };
    reference
        .identity_from(schema, control, &mut ReferenceWork { control, work })
        .await
}

#[cfg(test)]
mod bound_identity_tests {
    #[test]
    fn malformed_bound_names_yield_while_decoding_and_keep_the_admitted_original_diagnostic() {
        use uqa_core::{
            image_io::{ImageDecodeWindow, ImageDecoder},
            memory::{MemoryBudget, ProductionControl},
            CancellationToken,
        };
        let original = CancellationToken::new();
        let memory = MemoryBudget::new(1 << 20);
        let name = format!("\"{}", "資料".repeat(8_192));
        let expected = format!("internal error: decode bound relation `{name}`: unterminated quoted relation name `{name}`");
        let source = original.clone();
        let mut reader = ImageDecoder::new(&memory, &original, move |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(input.memory, &source, input.cancellation);
            super::bound_relation_identity_with_work(&name, &control, &mut input).await
        })
        .unwrap();
        let mut polls = 0;
        let error = loop {
            polls += 1;
            match reader.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            ) {
                Err(error) => break error,
                Ok(None) => assert!(polls < 20_000),
                Ok(Some(_)) => panic!("unterminated reference accepted"),
            }
        };
        assert!(polls > 16);
        assert_eq!(error.to_string(), expected);
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert!(matches!(error.primary(), crate::SQLError::Internal(_)));
        drop(reader);
        assert!(memory.used() >= expected.len() - "internal error: ".len());
        drop(error);
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn controlled_bound_identity_retains_admission_and_original_cancellation() {
        use uqa_core::{
            memory::{MemoryBudget, ProductionControl},
            CancellationToken,
        };
        let memory = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let relation = super::bound_relation_identity_with_control(
            "\"quoted\"\"schema\".\"quoted\"\"table\"",
            &control,
        )
        .unwrap();
        assert_eq!(relation.schema, "quoted\"schema");
        assert_eq!(relation.name, "quoted\"table");
        assert!(memory.used() > 0);
        drop(relation);
        assert_eq!(memory.used(), 0);
        let empty = MemoryBudget::new(0);
        assert!(super::bound_relation_identity_with_control(
            "public.items",
            &ProductionControl::new(&empty, &original, &invoking)
        )
        .is_err());
        original.cancel();
        assert!(super::bound_relation_identity_with_control("public.items", &control).is_err());
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn bound_identity_requires_a_schema_and_keeps_quoted_names_and_schema_aliases() {
        for (input, schema, name) in [
            ("public.items", "public", "items"),
            ("pg_temp.items", "pg_temp", "items"),
            (
                "\"other.schema\".\"odd.table\"",
                "other.schema",
                "odd.table",
            ),
        ] {
            assert_eq!(
                super::bound_relation_identity(input).unwrap(),
                uqa_core::RelationIdentity::new(schema, name)
            );
        }
        let error = super::bound_relation_identity("items").unwrap_err();
        assert_eq!(
            error.to_string(),
            "internal error: bound query contains non-canonical relation reference `items`"
        );
        assert!(super::bound_relation_identity("a.b.c").is_err());
    }
}
