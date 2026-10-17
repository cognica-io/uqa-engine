//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extension membership of the bundled procedural language and its handlers.

use super::DependencyBuilder;
use uqa_sql::catalog::{
    dependencies::{
        DependencyKind, ObjectAddress, EXTENSION_CLASS, LANGUAGE_CLASS, PROCEDURE_CLASS,
    },
    languages::{
        PLPGSQL_CALL_HANDLER, PLPGSQL_EXTENSION, PLPGSQL_INLINE_HANDLER, PLPGSQL_LANGUAGE,
        PLPGSQL_VALIDATOR,
    },
};

impl DependencyBuilder<'_> {
    pub(super) fn record_bundled_extensions(&mut self) {
        let extension = ObjectAddress::whole(EXTENSION_CLASS, PLPGSQL_EXTENSION);
        for member in [
            ObjectAddress::whole(PROCEDURE_CLASS, PLPGSQL_CALL_HANDLER),
            ObjectAddress::whole(PROCEDURE_CLASS, PLPGSQL_INLINE_HANDLER),
            ObjectAddress::whole(PROCEDURE_CLASS, PLPGSQL_VALIDATOR),
            ObjectAddress::whole(LANGUAGE_CLASS, PLPGSQL_LANGUAGE),
        ] {
            self.recorder
                .record(member, extension, DependencyKind::Extension);
        }
    }
}
