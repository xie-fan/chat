-- This marker is written in the same transaction as the restored data.
-- A separate schema keeps it out of the archived public tables.
CREATE SCHEMA IF NOT EXISTS chat_internal;
CREATE TABLE chat_internal.backup_restore (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    operation_id UUID
);
INSERT INTO chat_internal.backup_restore DEFAULT VALUES;
