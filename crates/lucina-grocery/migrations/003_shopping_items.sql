-- Shopping item objects (contract: SecretaryIOS docs/shopping-v2.md, "Item objects").
-- Additive and idempotent: safe to run more than once. Reverse with
-- migrations/003_shopping_items.down.sql.
--
-- * Every list row points to an item object: a master catalog id ("milk") or
--   a custom object ("u_<ulid>") owned by one user, with names in it/en/da.
-- * grocery.shopping_custom_items: the custom objects. `terms` holds every
--   diacritic-folded name and alias (SecretaryIOS docs/catalog.md normalise),
--   written by the app, so typed text in any language finds the same object.
-- * shopping_list_items.item_id: the object id. Rows that already have a
--   catalog id get it here; the rest are converted by
--   scripts/grocery-migrate-items.mjs (custom objects need translations).
--   raw_name stays the text as typed (API: typedName).
-- * One active row per (list, coalesce(item_id, catalog_item_id, name_key)).
--   For every row that exists when this runs the key is the 002 key, so the
--   new unique index cannot fail on existing data.
BEGIN;

CREATE TABLE IF NOT EXISTS grocery.shopping_custom_items (
  id text PRIMARY KEY CHECK (id ~ '^u_[0-9A-HJKMNP-TV-Z]{26}$'),
  user_id text NOT NULL,
  names jsonb NOT NULL CHECK (jsonb_typeof(names) = 'object'),
  aliases jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(aliases) = 'array'),
  terms text[] NOT NULL DEFAULT '{}',
  section text NOT NULL DEFAULT 'other',
  icon text,
  needs_translation boolean NOT NULL DEFAULT false,
  created_by text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS shopping_custom_items_user_idx ON grocery.shopping_custom_items (user_id);
CREATE INDEX IF NOT EXISTS shopping_custom_items_terms_idx ON grocery.shopping_custom_items USING gin (terms);

ALTER TABLE grocery.shopping_list_items ADD COLUMN IF NOT EXISTS item_id text;

UPDATE grocery.shopping_list_items
   SET item_id = catalog_item_id
 WHERE item_id IS NULL AND catalog_item_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS shopping_list_items_one_active_per_item
  ON grocery.shopping_list_items (list_id, (COALESCE(item_id, catalog_item_id, name_key)))
  WHERE status <> 'removed';
DROP INDEX IF EXISTS grocery.shopping_list_items_one_active_per_key;
CREATE INDEX IF NOT EXISTS shopping_list_items_item_idx ON grocery.shopping_list_items (item_id);

COMMIT;
