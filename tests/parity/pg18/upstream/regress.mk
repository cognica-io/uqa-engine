# Build the unmodified PostgreSQL test helper with its installed PGXS rules.
MODULE_big = regress
OBJS = regress.o
VPATH = /opt/postgresql-18.4/src/test/regress
PG_CONFIG = /usr/lib/postgresql/18/bin/pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)
