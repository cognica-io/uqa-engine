\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
BEGIN;
CREATE SCHEMA uqa_not_null_origin;
SET LOCAL search_path = uqa_not_null_origin, pg_catalog;
CREATE FUNCTION pg_temp.origin_state(label text, target regclass)
RETURNS TABLE(record text, column_name name, constraint_name name, local boolean, parents integer, validated boolean)
LANGUAGE SQL AS $oracle$
    SELECT label, a.attname, c.conname, c.conislocal, c.coninhcount, c.convalidated
    FROM pg_constraint c JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=ANY(c.conkey)
    WHERE c.conrelid=target AND c.contype='n' ORDER BY a.attnum
$oracle$;
CREATE FUNCTION pg_temp.origin_probe(label text, command text)
RETURNS text LANGUAGE plpgsql AS $oracle$
DECLARE state text; message text;
BEGIN
    EXECUTE command;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

CREATE TABLE origin_parent(a integer CONSTRAINT parent_nn NOT NULL);
CREATE TABLE origin_inherited() INHERITS(origin_parent);
CREATE TABLE origin_redeclared(a integer) INHERITS(origin_parent);
CREATE TABLE origin_local(a integer NOT NULL) INHERITS(origin_parent);
CREATE TABLE origin_named(a integer CONSTRAINT child_nn NOT NULL) INHERITS(origin_parent);
SELECT * FROM pg_temp.origin_state('declared-parent', 'origin_parent');
SELECT * FROM pg_temp.origin_state('inherited-only', 'origin_inherited');
SELECT * FROM pg_temp.origin_state('nullable-redeclaration', 'origin_redeclared');
SELECT * FROM pg_temp.origin_state('local-not-null', 'origin_local');
SELECT * FROM pg_temp.origin_state('named-local-not-null', 'origin_named');
ALTER TABLE origin_inherited ALTER COLUMN a SET NOT NULL;
SELECT * FROM pg_temp.origin_state('explicit-set-becomes-local', 'origin_inherited');

CREATE TABLE set_parent(a integer);
CREATE TABLE set_local(a integer CONSTRAINT retained_nn NOT NULL) INHERITS(set_parent);
CREATE TABLE set_inherited() INHERITS(set_parent);
ALTER TABLE set_parent ALTER COLUMN a SET NOT NULL;
SELECT * FROM pg_temp.origin_state('recursive-set-parent', 'set_parent');
SELECT * FROM pg_temp.origin_state('recursive-set-local', 'set_local');
SELECT * FROM pg_temp.origin_state('recursive-set-inherited', 'set_inherited');
CREATE TABLE add_parent(a integer);
CREATE TABLE add_local(a integer CONSTRAINT retained_nn NOT NULL) INHERITS(add_parent);
CREATE TABLE add_inherited() INHERITS(add_parent);
ALTER TABLE add_parent ADD CONSTRAINT requested_nn NOT NULL a;
SELECT * FROM pg_temp.origin_state('recursive-add-parent', 'add_parent');
SELECT * FROM pg_temp.origin_state('recursive-add-local', 'add_local');
SELECT * FROM pg_temp.origin_state('recursive-add-inherited', 'add_inherited');
CREATE TABLE column_parent(a integer);
CREATE TABLE column_local(b integer CONSTRAINT retained_nn NOT NULL) INHERITS(column_parent);
CREATE TABLE column_inherited() INHERITS(column_parent);
ALTER TABLE column_parent ADD COLUMN b integer NOT NULL;
SELECT * FROM pg_temp.origin_state('add-column-local', 'column_local');
SELECT * FROM pg_temp.origin_state('add-column-inherited', 'column_inherited');

CREATE TABLE origin_left(a integer CONSTRAINT left_nn NOT NULL);
CREATE TABLE origin_right(a integer CONSTRAINT right_nn NOT NULL);
CREATE TABLE origin_shared() INHERITS(origin_left, origin_right);
SELECT * FROM pg_temp.origin_state('two-parents', 'origin_shared');
ALTER TABLE origin_shared NO INHERIT origin_right;
SELECT * FROM pg_temp.origin_state('one-parent', 'origin_shared');
SAVEPOINT origin_change;
ALTER TABLE origin_shared NO INHERIT origin_left;
SELECT * FROM pg_temp.origin_state('last-parent-removed', 'origin_shared');
ROLLBACK TO origin_change;
SELECT * FROM pg_temp.origin_state('removal-rolled-back', 'origin_shared');
CREATE TABLE origin_partitioned(a integer CONSTRAINT partition_nn NOT NULL) PARTITION BY RANGE(a);
CREATE TABLE origin_born PARTITION OF origin_partitioned FOR VALUES FROM(0) TO(10);
CREATE TABLE origin_attached(a integer CONSTRAINT attached_nn NOT NULL);
SELECT * FROM pg_temp.origin_state('before-attach', 'origin_attached');
ALTER TABLE origin_partitioned ATTACH PARTITION origin_attached FOR VALUES FROM(10) TO(20);
SELECT * FROM pg_temp.origin_state('partition-born', 'origin_born');
SELECT * FROM pg_temp.origin_state('partition-attached', 'origin_attached');
ALTER TABLE origin_partitioned DETACH PARTITION origin_born;
SELECT * FROM pg_temp.origin_state('partition-detached', 'origin_born');

CREATE TABLE validation_parent(a integer);
CREATE TABLE validation_child() INHERITS(validation_parent);
INSERT INTO validation_child VALUES(NULL);
ALTER TABLE validation_parent ADD CONSTRAINT retained_nn NOT NULL a NOT VALID;
SELECT * FROM pg_temp.origin_state('inherited-unvalidated', 'validation_child');
ALTER TABLE validation_child ALTER COLUMN a SET NOT NULL;
SELECT * FROM pg_temp.origin_state('local-unvalidated', 'validation_child');
SELECT pg_temp.origin_probe('repeat-set-validates', 'ALTER TABLE validation_child ALTER COLUMN a SET NOT NULL');
SELECT * FROM pg_temp.origin_state('failed-validation-preserves-local', 'validation_child');
DELETE FROM validation_child;
ALTER TABLE validation_child ALTER COLUMN a SET NOT NULL;
SELECT * FROM pg_temp.origin_state('local-validated', 'validation_child');
CREATE TABLE no_inherit_parent(a integer CONSTRAINT local_nn NOT NULL NO INHERIT);
CREATE TABLE no_inherit_child() INHERITS(no_inherit_parent);
INSERT INTO no_inherit_child VALUES(NULL);
SELECT 'no-inherit-child-constraints', count(*) FROM pg_constraint WHERE conrelid='no_inherit_child'::regclass;
SELECT 'no-inherit-child-null', count(*) FROM no_inherit_child WHERE a IS NULL;
ROLLBACK;
