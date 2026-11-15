-- PostgreSQL 18 reference for captured rule row bindings and later column changes.
CREATE TABLE items(id integer, value integer, discarded integer);
CREATE TABLE lookup(id integer);
CREATE TABLE rule_log(entry text);
CREATE DOMAIN rule_truth AS boolean;
CREATE FUNCTION rule_never() RETURNS boolean LANGUAGE SQL AS 'SELECT 1 / 0 > 0';
INSERT INTO lookup VALUES(2);
CREATE RULE inserted AS ON INSERT TO items WHERE EXISTS(SELECT 1 FROM lookup AS new WHERE new.id=2) DO ALSO INSERT INTO rule_log VALUES('insert:' || NEW.id);
CREATE RULE changed AS ON UPDATE TO items WHERE EXISTS(WITH delta AS (SELECT NEW.value - OLD.value AS amount) SELECT 1 FROM delta, lookup WHERE amount>0 AND lookup.id=NEW.id)::rule_truth DO ALSO INSERT INTO rule_log VALUES('change:' || OLD.value || ':' || NEW.value);
CREATE RULE quiet AS ON UPDATE TO items WHERE false AND rule_never() DO ALSO INSERT INTO rule_log VALUES('invalid eager call');
CREATE RULE retained AS ON DELETE TO items WHERE OLD.id IN (SELECT id FROM lookup) DO INSTEAD INSERT INTO rule_log VALUES('retain:' || OLD.id);
CREATE RULE whole AS ON UPDATE TO items WHERE OLD IS DISTINCT FROM NEW DO ALSO INSERT INTO rule_log VALUES('whole:' || to_jsonb(NEW)::text);
INSERT INTO items VALUES(1,10,0),(2,20,0);
ALTER TABLE items RENAME COLUMN id TO item_id;
ALTER TABLE items DROP COLUMN discarded;
ALTER TABLE items ADD COLUMN extra text DEFAULT 'later';
ALTER DOMAIN rule_truth RENAME TO rule_renamed_truth;
ALTER FUNCTION rule_never() RENAME TO rule_renamed_never;
TRUNCATE rule_log;
-- capture boundary
UPDATE items SET value=value+1;
DELETE FROM items;
INSERT INTO items VALUES(3,30,'after');
SELECT string_agg(entry, E'\n' ORDER BY entry) AS observations FROM rule_log;
