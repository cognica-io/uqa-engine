//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained routine images preserve the existing ordered definition/body encoding.

use super::CapturedRoutines;
use crate::routines::{CompiledFunctionBody, RoutineBody, SQLRoutine};
use serde::{ser::SerializeSeq, Serialize, Serializer};

impl Serialize for CapturedRoutines {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries = serializer.serialize_seq(Some(self.len()))?;
        for (name, overloads) in self {
            entries.serialize_element(&(name, CapturedOverloads(overloads)))?;
        }
        entries.end()
    }
}

struct CapturedOverloads<'a>(&'a [SQLRoutine]);

impl Serialize for CapturedOverloads<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Entry<'a> {
            definition: &'a crate::ast::CreateFunction,
            bound_body: Option<&'a Vec<crate::plan::UnifiedPlan>>,
        }
        let mut entries = serializer.serialize_seq(Some(self.0.len()))?;
        for routine in self.0 {
            let bound_body = match &routine.body {
                RoutineBody::Source => None,
                RoutineBody::Bound(body) => match &***body {
                    CompiledFunctionBody::SQL(plans) => Some(plans),
                    CompiledFunctionBody::PLpgSQL(_) => {
                        return Err(serde::ser::Error::custom(
                            "stored routine contains a session-compiled PL/pgSQL body",
                        ));
                    }
                },
            };
            entries.serialize_element(&Entry {
                definition: &routine.def,
                bound_body,
            })?;
        }
        entries.end()
    }
}
