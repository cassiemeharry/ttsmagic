ALTER TABLE deck ADD COLUMN created_at timestamptz NULL DEFAULT NULL;

UPDATE deck SET created_at = now() WHERE created_at IS NULL;

ALTER TABLE deck ALTER created_at SET NOT NULL;
