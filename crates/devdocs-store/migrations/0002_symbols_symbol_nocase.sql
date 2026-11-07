-- Schema 0002: NOCASE index for the case-insensitive symbol ladder.
--
-- Source of truth: docs/implementation/06-data-model.md §Schema.
-- Applied migrations never change: a schema change is a new migration file
-- (docs/implementation/06 §Migrations).
--
-- The case-insensitive mode matches `sym.symbol LIKE ?3 ESCAPE '\'` (the
-- exact match is covered by the prefix pattern and ordered first through
-- the match-quality expression). Under the default case-insensitive LIKE,
-- the prefix range needs an index on NOCASE collation; without it the
-- planner scans the symbol table. The column collation stays BINARY:
-- exact mode keeps matching byte-exact.

CREATE INDEX idx_symbols_symbol_nocase ON symbols(symbol COLLATE NOCASE);
