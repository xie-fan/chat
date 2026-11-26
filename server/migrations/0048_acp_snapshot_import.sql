-- A completed transfer can be acknowledged again if the desktop lost its response.
ALTER TABLE acp_hosts ADD COLUMN snapshot_import_id uuid;
