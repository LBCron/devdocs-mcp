-- Schema 0002 for the devdocs store: the linked-page URL on chunks.
--
-- Source of truth: docs/implementation/06-data-model.md §Schema. Applied
-- migrations never change: a schema change is a new migration file
-- (docs/implementation/06 §Migrations).

ALTER TABLE chunks ADD COLUMN page_url TEXT;
