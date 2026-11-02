-- Schema version 6: the app's own key-value state. One row per key,
-- the value a JSON text — the first key is the expanded-rows set the
-- user chose to persist across sessions. Nothing here is swept data:
-- the app wrote it, the app reads it back.

CREATE TABLE app_state (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
