#ifndef PG_QUERY_PLPGSQL_CATALOG_H
#define PG_QUERY_PLPGSQL_CATALOG_H

#include "pg_query.h"

#include <stdbool.h>
#include <stdint.h>

bool pg_query_plpgsql_catalog_available(void);
bool pg_query_plpgsql_lookup_namespace(const char* schema_name,
                                       uint32_t* namespace_oid);
bool pg_query_plpgsql_lookup_type_by_name(const char* schema_name,
                                          const char* type_name,
                                          PgQueryPlpgsqlTypeMetadata* type);
bool pg_query_plpgsql_lookup_type_by_oid(uint32_t type_oid,
                                         PgQueryPlpgsqlTypeMetadata* type);

#endif
