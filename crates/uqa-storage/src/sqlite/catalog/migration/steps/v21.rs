//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent B-tree consistency repair markers and structural guards.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _btree_indexes (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        PRIMARY KEY (table_name, field)
    );
    CREATE TABLE IF NOT EXISTS _btree_index_entries (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        value_json TEXT NOT NULL,
        PRIMARY KEY (table_name, field, doc_id),
        FOREIGN KEY (table_name, field)
            REFERENCES _btree_indexes (table_name, field)
            ON UPDATE CASCADE ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS _btree_index_value_idx
        ON _btree_index_entries (table_name, field, value_json, doc_id);
    CREATE INDEX IF NOT EXISTS _btree_index_doc_idx
        ON _btree_index_entries (table_name, doc_id);
    CREATE TABLE IF NOT EXISTS _btree_index_repairs (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        PRIMARY KEY (table_name, field)
    ) WITHOUT ROWID;

    CREATE TEMP TABLE _btree_v21_invalid (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        PRIMARY KEY (table_name, field)
    ) WITHOUT ROWID;

    INSERT OR IGNORE INTO _btree_v21_invalid (table_name, field)
        SELECT entry.table_name, entry.field
          FROM _btree_index_entries AS entry
          LEFT JOIN _btree_indexes AS marker
            ON marker.table_name = entry.table_name
           AND marker.field = entry.field
          LEFT JOIN _documents AS document
            ON document.table_name = entry.table_name
           AND document.doc_id = entry.doc_id
         WHERE marker.field IS NULL OR document.doc_id IS NULL;

    INSERT OR IGNORE INTO _btree_v21_invalid (table_name, field)
        SELECT marker.table_name, marker.field
          FROM _btree_indexes AS marker
          JOIN _documents AS document
            ON document.table_name = marker.table_name
          LEFT JOIN _btree_index_entries AS entry
            ON entry.table_name = marker.table_name
           AND entry.field = marker.field
           AND entry.doc_id = document.doc_id
         WHERE entry.doc_id IS NULL;

    INSERT OR IGNORE INTO _btree_index_repairs (table_name, field)
        SELECT table_name, field FROM _btree_v21_invalid;

    DELETE FROM _btree_index_entries
     WHERE NOT EXISTS (
         SELECT 1 FROM _btree_indexes AS marker
          WHERE marker.table_name = _btree_index_entries.table_name
            AND marker.field = _btree_index_entries.field
     ) OR NOT EXISTS (
         SELECT 1 FROM _documents AS document
          WHERE document.table_name = _btree_index_entries.table_name
            AND document.doc_id = _btree_index_entries.doc_id
     );

    CREATE TRIGGER IF NOT EXISTS _btree_documents_delete
        AFTER DELETE ON _documents
        BEGIN
            DELETE FROM _btree_index_entries
             WHERE table_name = OLD.table_name AND doc_id = OLD.doc_id;
        END;

    CREATE TRIGGER IF NOT EXISTS _btree_entries_document_insert
        BEFORE INSERT ON _btree_index_entries
        WHEN NOT EXISTS (
            SELECT 1 FROM _documents
             WHERE table_name = NEW.table_name AND doc_id = NEW.doc_id
        )
        BEGIN
            SELECT RAISE(ABORT, 'persistent B-tree entry has no backing document');
        END;

    CREATE TRIGGER IF NOT EXISTS _btree_entries_document_update
        BEFORE UPDATE OF table_name, doc_id ON _btree_index_entries
        WHEN NOT EXISTS (
            SELECT 1 FROM _documents
             WHERE table_name = NEW.table_name AND doc_id = NEW.doc_id
        )
        BEGIN
            SELECT RAISE(ABORT, 'persistent B-tree entry has no backing document');
        END;

    CREATE TRIGGER IF NOT EXISTS _btree_documents_doc_id_update
        AFTER UPDATE OF doc_id ON _documents
        WHEN OLD.doc_id <> NEW.doc_id
        BEGIN
            UPDATE _btree_index_entries
               SET doc_id = NEW.doc_id
             WHERE table_name = OLD.table_name AND doc_id = OLD.doc_id;
        END;

    DROP TABLE _btree_v21_invalid;
    ";
