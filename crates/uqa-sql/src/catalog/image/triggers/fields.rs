//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Fields;
use crate::{
    ast::{CreateTrigger, EventEnableMode, TriggerDeferrability, TriggerTiming},
    catalog::events::StoredTrigger,
};

impl<'a> Fields<'a> {
    pub(super) fn trigger(&mut self, value: &'a StoredTrigger) {
        let StoredTrigger {
            definition: _,
            function_object_id,
            enabled,
            object_id,
            constraint_name,
            catalog_oid,
            constraint_catalog_oid,
        } = value;
        self.optional_incarnation(*function_object_id);
        self.scalar([match enabled {
            EventEnableMode::Origin => 0,
            EventEnableMode::Disabled => 1,
            EventEnableMode::Replica => 2,
            EventEnableMode::Always => 3,
        }]);
        self.optional_incarnation(*object_id);
        self.optional_text(constraint_name.as_deref());
        for oid in [catalog_oid, constraint_catalog_oid] {
            self.flag(oid.is_some());
            if let Some(oid) = oid {
                self.scalar(oid.to_le_bytes());
            }
        }
    }

    pub(super) fn trigger_definition(&mut self, value: &'a CreateTrigger) {
        let CreateTrigger {
            name,
            table,
            function,
            arguments,
            constraint,
            referenced_table,
            deferrability,
            row,
            timing,
            events,
            update_columns,
            transition_relations,
            when,
            or_replace,
        } = value;
        self.text(name);
        self.text(table);
        self.text(function);
        self.flag(*constraint);
        self.optional_text(referenced_table.as_deref());
        self.scalar([match deferrability {
            TriggerDeferrability::NotDeferrable => 0,
            TriggerDeferrability::InitiallyImmediate => 1,
            TriggerDeferrability::InitiallyDeferred => 2,
        }]);
        self.flag(*row);
        self.scalar([match timing {
            TriggerTiming::Before => 0,
            TriggerTiming::After => 1,
            TriggerTiming::InsteadOf => 2,
        }]);
        self.flag(*or_replace);
        self.flag(when.is_some());
        self.count(arguments.len());
        self.count(events.len());
        self.count(update_columns.len());
        self.count(transition_relations.len());
    }
}
