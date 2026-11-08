//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required component prefixes and one selected generation per relation follow the wire order.

use super::{BackupError, BackupResult, DecodedMemoryImageAddress as Address, UnitKind};
use crate::backup::{
    DecodedMemoryTableUnit as TableUnit, MEMORY_IMAGE_CATALOG_COUNT, MEMORY_IMAGE_RESOURCE_KINDS,
    MEMORY_IMAGE_TABLE_METADATA_COUNT,
};

pub(super) fn end(previous: Option<&Address>) -> BackupResult<()> {
    match previous {
        None => Err(invalid()),
        Some(Address::Catalog(slot)) if *slot != MEMORY_IMAGE_CATALOG_COUNT => Err(invalid()),
        Some(Address::Table {
            unit: TableUnit::Metadata(slot),
            ..
        }) if *slot != MEMORY_IMAGE_TABLE_METADATA_COUNT - 1 => Err(invalid()),
        _ => Ok(()),
    }
}

pub(super) fn next(
    previous: Option<&Address>,
    current: &Address,
    kind: UnitKind,
) -> BackupResult<()> {
    let expected_catalog = match previous {
        None => Some(1),
        Some(Address::Catalog(slot)) if *slot < MEMORY_IMAGE_CATALOG_COUNT => Some(slot + 1),
        _ => None,
    };
    if let Some(expected) = expected_catalog {
        return if matches!(current, Address::Catalog(slot) if *slot == expected)
            && kind == UnitKind::Metadata
        {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    let expected_kind = match current {
        Address::FieldAnalyzer { .. } => UnitKind::Resource,
        Address::Resource { kind, .. } if (1..=MEMORY_IMAGE_RESOURCE_KINDS).contains(kind) => {
            UnitKind::Resource
        }
        Address::Header | Address::Catalog(_) | Address::Resource { .. } => return Err(invalid()),
        Address::Table { unit, .. } => match unit {
            TableUnit::Metadata(slot) if *slot < MEMORY_IMAGE_TABLE_METADATA_COUNT => {
                UnitKind::Metadata
            }
            TableUnit::Metadata(_) => return Err(invalid()),
            TableUnit::Vector(_) => UnitKind::Artifact,
            TableUnit::ValueIndex { document: None, .. } => UnitKind::Metadata,
            TableUnit::Document(_)
            | TableUnit::Text(_)
            | TableUnit::ValueIndex {
                document: Some(_), ..
            } => UnitKind::Record,
        },
    };
    if kind != expected_kind {
        return Err(invalid());
    }
    let Address::Table {
        schema,
        relation,
        object,
        generation,
        unit,
    } = current
    else {
        return end(previous);
    };
    let previous_unit = match previous {
        Some(Address::Table {
            schema: before_schema,
            relation: before_relation,
            object: before_object,
            generation: before_generation,
            unit: before_unit,
        }) if **schema == **before_schema && **relation == **before_relation => {
            if object != before_object || generation != before_generation {
                return Err(invalid());
            }
            Some(before_unit)
        }
        _ => {
            end(previous)?;
            None
        }
    };
    match previous_unit {
        None if !matches!(unit, TableUnit::Metadata(0)) => return Err(invalid()),
        Some(TableUnit::Metadata(slot)) if *slot < MEMORY_IMAGE_TABLE_METADATA_COUNT - 1 => {
            return if matches!(unit, TableUnit::Metadata(next) if *next == slot + 1) {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        Some(_) if matches!(unit, TableUnit::Metadata(_)) => return Err(invalid()),
        _ => (),
    }
    if let TableUnit::ValueIndex {
        name,
        document: Some(_),
    } = unit
    {
        if !matches!(previous_unit, Some(TableUnit::ValueIndex { name: before, .. }) if **before == **name)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn invalid() -> BackupError {
    BackupError::Invalid("memory image component membership")
}
