-- Stateful PostgreSQL 18.4 constraint-lifecycle parity fixture.
-- The runner replaces __UQA_STATEFUL_SCHEMA__ and executes each delimited case in order.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case create_parent ok
CREATE TABLE parent (id INTEGER PRIMARY KEY);
-- @end

-- @case create_child ok
CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER, score INTEGER);
-- @end

-- @case insert_legacy_invalid_rows ok
INSERT INTO child VALUES (1, 99, -1), (2, NULL, NULL);
-- @end

-- @case add_check_not_valid ok
ALTER TABLE child ADD CONSTRAINT score_positive CHECK (score > 0) NOT VALID;
-- @end

-- @case check_not_valid_catalog rows
SELECT contype, conenforced, convalidated, connoinherit FROM pg_catalog.pg_constraint WHERE conname = 'score_positive';
-- @end

-- @case check_not_valid_enforces_new_rows error
INSERT INTO child VALUES (3, NULL, -2);
-- @end

-- @case check_validation_failure_is_atomic error
ALTER TABLE child VALIDATE CONSTRAINT score_positive;
-- @end

-- @case check_stays_not_valid_after_failure rows
SELECT conenforced, convalidated FROM pg_catalog.pg_constraint WHERE conname = 'score_positive';
-- @end

-- @case fix_legacy_check_row ok
UPDATE child SET score = 1 WHERE id = 1;
-- @end

-- @case validate_check ok
ALTER TABLE child VALIDATE CONSTRAINT score_positive;
-- @end

-- @case check_valid_catalog rows
SELECT conenforced, convalidated FROM pg_catalog.pg_constraint WHERE conname = 'score_positive';
-- @end

-- @case add_fk_not_valid ok
ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) NOT VALID;
-- @end

-- @case fk_not_valid_catalog rows
SELECT contype, condeferrable, condeferred, conenforced, convalidated, connoinherit FROM pg_catalog.pg_constraint WHERE conname = 'child_parent_fk';
-- @end

-- @case fk_not_valid_enforces_new_rows error
INSERT INTO child VALUES (4, 100, 5);
-- @end

-- @case fk_validation_failure_is_atomic error
ALTER TABLE child VALIDATE CONSTRAINT child_parent_fk;
-- @end

-- @case insert_missing_parent ok
INSERT INTO parent VALUES (99);
-- @end

-- @case validate_fk ok
ALTER TABLE child VALIDATE CONSTRAINT child_parent_fk;
-- @end

-- @case disable_fk ok
ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT ENFORCED;
-- @end

-- @case disabled_fk_catalog rows
SELECT conenforced, convalidated FROM pg_catalog.pg_constraint WHERE conname = 'child_parent_fk';
-- @end

-- @case disabled_fk_allows_invalid_row ok
INSERT INTO child VALUES (5, 12345, 5);
-- @end

-- @case enabling_invalid_fk_is_atomic error
ALTER TABLE child ALTER CONSTRAINT child_parent_fk ENFORCED;
-- @end

-- @case fk_stays_disabled_after_failure rows
SELECT conenforced, convalidated FROM pg_catalog.pg_constraint WHERE conname = 'child_parent_fk';
-- @end

-- @case delete_disabled_fk_row ok
DELETE FROM child WHERE id = 5;
-- @end

-- @case enable_fk ok
ALTER TABLE child ALTER CONSTRAINT child_parent_fk ENFORCED;
-- @end

-- @case make_fk_initially_deferred ok
ALTER TABLE child ALTER CONSTRAINT child_parent_fk DEFERRABLE INITIALLY DEFERRED;
-- @end

-- @case deferred_fk_catalog rows
SELECT condeferrable, condeferred, conenforced, convalidated FROM pg_catalog.pg_constraint WHERE conname = 'child_parent_fk';
-- @end

-- @case deferred_child_before_parent ok
BEGIN; INSERT INTO child VALUES (6, 777, 5); INSERT INTO parent VALUES (777); COMMIT;
-- @end

-- @case deferred_missing_parent_fails_commit error
BEGIN; INSERT INTO child VALUES (7, 888, 5); COMMIT;
-- @end

-- @case deferred_commit_failure_rolls_back rows
SELECT count(*) FROM child WHERE id = 7;
-- @end

-- @case deferred_parent_delete_reinsert ok
BEGIN; DELETE FROM parent WHERE id = 99; INSERT INTO parent VALUES (99); COMMIT;
-- @end

-- @case deferred_parent_delete_fails_commit error
BEGIN; DELETE FROM parent WHERE id = 99; COMMIT;
-- @end

-- @case deferred_parent_failure_rolls_back rows
SELECT count(*) FROM parent WHERE id = 99;
-- @end

-- @case deferred_savepoint_rollback ok
BEGIN; SAVEPOINT before_bad_child; INSERT INTO child VALUES (8, 999, 5); ROLLBACK TO SAVEPOINT before_bad_child; COMMIT;
-- @end

-- @case restore_immediate_fk ok
ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT DEFERRABLE INITIALLY IMMEDIATE;
-- @end

-- @case invalid_check_deferrability error
ALTER TABLE child ALTER CONSTRAINT score_positive DEFERRABLE;
-- @end

-- @case invalid_fk_inheritability error
ALTER TABLE child ALTER CONSTRAINT child_parent_fk NO INHERIT;
-- @end

-- @case invalid_not_valid_transition error
ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT VALID;
-- @end

-- @case create_named_not_null_target ok
CREATE TABLE named_nn (id INTEGER PRIMARY KEY, value INTEGER);
-- @end

-- @case insert_legacy_null ok
INSERT INTO named_nn VALUES (1, NULL);
-- @end

-- @case add_named_not_null_not_valid ok
ALTER TABLE named_nn ADD CONSTRAINT value_nn NOT NULL value NOT VALID;
-- @end

-- @case named_not_null_catalog rows
SELECT c.contype, c.conenforced, c.convalidated, c.connoinherit, a.attnotnull FROM pg_catalog.pg_constraint AS c JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey) WHERE c.conname = 'value_nn';
-- @end

-- @case named_not_null_enforces_new_rows error
INSERT INTO named_nn VALUES (2, NULL);
-- @end

-- @case named_not_null_validation_failure error
ALTER TABLE named_nn VALIDATE CONSTRAINT value_nn;
-- @end

-- @case fix_legacy_null ok
UPDATE named_nn SET value = 7 WHERE id = 1;
-- @end

-- @case validate_named_not_null ok
ALTER TABLE named_nn VALIDATE CONSTRAINT value_nn;
-- @end

-- @case set_named_not_null_no_inherit ok
ALTER TABLE named_nn ALTER CONSTRAINT value_nn NO INHERIT;
-- @end

-- @case named_not_null_valid_catalog rows
SELECT convalidated, connoinherit FROM pg_catalog.pg_constraint WHERE conname = 'value_nn';
-- @end

-- @case drop_named_not_null ok
ALTER TABLE named_nn DROP CONSTRAINT value_nn;
-- @end

-- @case dropped_not_null_attribute rows
SELECT attnotnull FROM pg_catalog.pg_attribute WHERE attrelid = 'named_nn'::regclass AND attname = 'value';
-- @end

-- @case create_dependency_parent ok
CREATE TABLE dep_parent (id INTEGER CONSTRAINT dep_key UNIQUE);
-- @end

-- @case create_dependency_child ok
CREATE TABLE dep_child (parent_id INTEGER CONSTRAINT dep_fk REFERENCES dep_parent(id));
-- @end

-- @case drop_referenced_key_restrict error
ALTER TABLE dep_parent DROP CONSTRAINT dep_key RESTRICT;
-- @end

-- @case drop_referenced_key_cascade ok
ALTER TABLE dep_parent DROP CONSTRAINT dep_key CASCADE;
-- @end

-- @case cascade_keeps_child_and_drops_fk rows
SELECT to_regclass('__UQA_STATEFUL_SCHEMA__.dep_child') IS NOT NULL, EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conname = 'dep_fk');
-- @end

-- @case create_atomic_target ok
CREATE TABLE atomic_target (value INTEGER);
-- @end

-- @case insert_atomic_bad_row ok
INSERT INTO atomic_target VALUES (-1);
-- @end

-- @case multi_action_validation_is_atomic error
ALTER TABLE atomic_target ADD CONSTRAINT atomic_positive CHECK (value > 0) NOT VALID, VALIDATE CONSTRAINT atomic_positive;
-- @end

-- @case failed_multi_action_leaves_no_constraint rows
SELECT count(*) FROM pg_catalog.pg_constraint WHERE conrelid = 'atomic_target'::regclass;
-- @end

-- @case duplicate_constraint_name_is_atomic error
ALTER TABLE atomic_target ADD CONSTRAINT duplicate_name CHECK (value <> 0) NOT VALID, ADD CONSTRAINT duplicate_name CHECK (value <> 1) NOT VALID;
-- @end

-- @case duplicate_failure_leaves_no_constraint rows
SELECT count(*) FROM pg_catalog.pg_constraint WHERE conrelid = 'atomic_target'::regclass;
-- @end
