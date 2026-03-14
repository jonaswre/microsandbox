-- Add backend-neutral runtime columns to the sandboxes table.
-- These columns support the cross-platform runtime model alongside legacy PID columns.

ALTER TABLE sandboxes ADD COLUMN backend_kind TEXT NOT NULL DEFAULT 'unix_krun';
ALTER TABLE sandboxes ADD COLUMN runtime_id TEXT NOT NULL DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN control_endpoint TEXT NOT NULL DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN backend_object_id TEXT NOT NULL DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN rootfs_descriptor_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE sandboxes ADD COLUMN backend_state_json TEXT NOT NULL DEFAULT '{}';
