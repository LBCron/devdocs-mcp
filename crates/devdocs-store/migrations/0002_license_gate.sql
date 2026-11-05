-- Migration 0002: the license-gate expiry column.
--
-- Source of truth: docs/implementation/06 §Schema, docs/implementation/07
-- §License gate. Applied migrations never change: a schema change is a new
-- migration file (docs/implementation/06 §Migrations).
--
-- Adds the nullable expiry date the store sweep flips passed sources to
-- `expired` against. Nullable and non-destructive: existing sources carry
-- no expiry and keep their state.

ALTER TABLE sources ADD COLUMN license_expires_at TEXT;
