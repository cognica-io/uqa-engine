#ifndef PG_QUERY_PARSER_OPTIONS_H
#define PG_QUERY_PARSER_OPTIONS_H

#include "pg_query.h"
#include "parser/parser.h"

typedef struct PgQueryParserOptionsScope
{
	int backslash_quote;
	bool standard_conforming_strings;
	bool escape_string_warning;
} PgQueryParserOptionsScope;

static inline PgQueryParserOptionsScope
pg_query_begin_parser_options(int options)
{
	PgQueryParserOptionsScope previous = {
		backslash_quote, standard_conforming_strings, escape_string_warning
	};
	backslash_quote = (options & PG_QUERY_DISABLE_BACKSLASH_QUOTE)
		? BACKSLASH_QUOTE_OFF : BACKSLASH_QUOTE_SAFE_ENCODING;
	standard_conforming_strings = !(options & PG_QUERY_DISABLE_STANDARD_CONFORMING_STRINGS);
	escape_string_warning = !(options & PG_QUERY_DISABLE_ESCAPE_STRING_WARNING);
	return previous;
}

static inline void
pg_query_end_parser_options(PgQueryParserOptionsScope previous)
{
	backslash_quote = previous.backslash_quote;
	standard_conforming_strings = previous.standard_conforming_strings;
	escape_string_warning = previous.escape_string_warning;
}

#endif
