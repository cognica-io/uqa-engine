CREATE TABLE parents(a integer UNIQUE, b integer UNIQUE);
CREATE TABLE children(id integer PRIMARY KEY, a integer REFERENCES parents(a) ON UPDATE CASCADE, b integer REFERENCES parents(b) ON UPDATE CASCADE, body text);
CREATE TABLE audit(seq bigserial PRIMARY KEY, message text);
CREATE FUNCTION capture_row() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO audit(message) VALUES ('R:'||OLD.id||':'||OLD.a||':'||OLD.b||'>'||NEW.a||':'||NEW.b||':'||length(NEW.body)||':'||(SELECT count(*) FROM new_rows));
 RETURN NULL;
END $$;
CREATE FUNCTION capture_statement() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO audit(message) VALUES ('S:'||(SELECT count(*) FROM old_rows)||':'||(SELECT sum(a) FROM old_rows)||':'||(SELECT sum(b) FROM old_rows)||'>'||(SELECT sum(a) FROM new_rows)||':'||(SELECT sum(b) FROM new_rows));
 RETURN NULL;
END $$;
CREATE TRIGGER a_row AFTER UPDATE ON children REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION capture_row();
CREATE TRIGGER z_statement AFTER UPDATE ON children REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION capture_statement();
INSERT INTO parents VALUES (1,1),(2,2);
INSERT INTO children VALUES (1,1,1,repeat('x',32768)),(2,1,1,repeat('y',32768)),(3,2,2,repeat('z',32768));
