-- PostgreSQL 18 reference for creation-bound types after a rename and name replacement.
CREATE TYPE original_mood AS ENUM ('calm','busy');
CREATE FUNCTION mood_read(n text DEFAULT 'calm'::original_mood::text) RETURNS text LANGUAGE SQL BEGIN ATOMIC SELECT n::original_mood::text; END;
ALTER TYPE original_mood RENAME TO retained_mood;
CREATE TYPE original_mood AS ENUM ('replacement');
-- capture boundary
SELECT mood_read();
SELECT mood_read('busy');
