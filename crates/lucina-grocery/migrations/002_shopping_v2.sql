-- Shopping v2 (contract: SecretaryIOS docs/shopping-v2.md).
-- Additive and idempotent: safe to run more than once. Reverse with
-- migrations/002_shopping_v2.down.sql.
--
-- * grocery.shopping_lists: named lists per user; exactly one default list
--   per user (auto-created), a per-list `revision` bumped once per op batch.
-- * shopping_list_items gains list_id, quantity, unit, specification,
--   section_override, icon_key, catalog_item_id, position, purchased_at,
--   modified_by, rev (list revision of its last change) and name_key
--   (generated normalised name).
-- * status 'purchased' becomes 'recent' (needed | recent | removed).
-- * One active row per (list, coalesce(catalog_item_id, name_key)).
-- * grocery.shopping_ops: receipts that make op ids idempotent.
BEGIN;

CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE IF NOT EXISTS grocery.shopping_lists (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id text NOT NULL,
  name text NOT NULL,
  theme text NOT NULL DEFAULT 'home'
    CHECK (theme IN ('home','grocery','work','holiday','garden','hardware','baby','other')),
  position integer NOT NULL DEFAULT 0,
  is_default boolean NOT NULL DEFAULT false,
  revision bigint NOT NULL DEFAULT 0,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  deleted_at timestamptz
);
CREATE UNIQUE INDEX IF NOT EXISTS shopping_lists_one_default_per_user
  ON grocery.shopping_lists (user_id) WHERE is_default;
CREATE INDEX IF NOT EXISTS shopping_lists_user_idx ON grocery.shopping_lists (user_id) WHERE deleted_at IS NULL;

ALTER TABLE grocery.shopping_list_items
  ADD COLUMN IF NOT EXISTS list_id uuid REFERENCES grocery.shopping_lists(id),
  ADD COLUMN IF NOT EXISTS quantity numeric,
  ADD COLUMN IF NOT EXISTS unit text,
  ADD COLUMN IF NOT EXISTS specification text,
  ADD COLUMN IF NOT EXISTS section_override text,
  ADD COLUMN IF NOT EXISTS icon_key text,
  ADD COLUMN IF NOT EXISTS catalog_item_id text,
  ADD COLUMN IF NOT EXISTS position integer,
  ADD COLUMN IF NOT EXISTS purchased_at timestamptz,
  ADD COLUMN IF NOT EXISTS modified_by text,
  ADD COLUMN IF NOT EXISTS rev bigint NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS name_key text
    GENERATED ALWAYS AS (lower(btrim(regexp_replace(raw_name, '\s+', ' ', 'g')))) STORED;

-- Status: purchased -> recent.
ALTER TABLE grocery.shopping_list_items DROP CONSTRAINT IF EXISTS shopping_list_items_status_check;
UPDATE grocery.shopping_list_items
   SET status = 'recent', purchased_at = COALESCE(purchased_at, updated_at)
 WHERE status = 'purchased';
ALTER TABLE grocery.shopping_list_items
  ADD CONSTRAINT shopping_list_items_status_check CHECK (status IN ('needed','recent','removed'));

-- A default list for every user that has items, then move the items into it.
INSERT INTO grocery.shopping_lists (user_id, name, theme, is_default)
SELECT DISTINCT i.user_id, 'Casa', 'home', true
  FROM grocery.shopping_list_items i
 WHERE NOT EXISTS (SELECT 1 FROM grocery.shopping_lists l WHERE l.user_id = i.user_id AND l.is_default)
ON CONFLICT DO NOTHING;

UPDATE grocery.shopping_list_items i
   SET list_id = l.id
  FROM grocery.shopping_lists l
 WHERE i.list_id IS NULL AND l.user_id = i.user_id AND l.is_default;

ALTER TABLE grocery.shopping_list_items ALTER COLUMN list_id SET NOT NULL;

-- Positions in creation order, per list.
UPDATE grocery.shopping_list_items i
   SET position = ranked.rn
  FROM (SELECT id, row_number() OVER (PARTITION BY list_id ORDER BY created_at, id) AS rn
          FROM grocery.shopping_list_items WHERE position IS NULL) ranked
 WHERE i.id = ranked.id;

-- Existing duplicates (same list and key, not removed): keep one, preferring
-- a needed row, then the most recently updated; the rest become removed.
-- Skipped once 003_shopping_items has run (item_id exists): from then on
-- uniqueness is per item object (shopping_list_items_one_active_per_item),
-- and re-running this file must not collapse rows of different objects.
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                  WHERE table_schema = 'grocery' AND table_name = 'shopping_list_items' AND column_name = 'item_id') THEN
    UPDATE grocery.shopping_list_items i
       SET status = 'removed', updated_at = now()
      FROM (SELECT id, row_number() OVER (
                     PARTITION BY list_id, COALESCE(catalog_item_id, name_key)
                     ORDER BY (status = 'needed') DESC, updated_at DESC, id) AS rn
              FROM grocery.shopping_list_items WHERE status <> 'removed') ranked
     WHERE i.id = ranked.id AND ranked.rn > 1;

    CREATE UNIQUE INDEX IF NOT EXISTS shopping_list_items_one_active_per_key
      ON grocery.shopping_list_items (list_id, (COALESCE(catalog_item_id, name_key)))
      WHERE status <> 'removed';
  END IF;
END $$;
CREATE INDEX IF NOT EXISTS shopping_list_items_list_idx
  ON grocery.shopping_list_items (list_id, status);

CREATE TABLE IF NOT EXISTS grocery.shopping_ops (
  op_id uuid PRIMARY KEY,
  list_id uuid NOT NULL REFERENCES grocery.shopping_lists(id),
  user_id text NOT NULL,
  op_type text NOT NULL,
  outcome text NOT NULL CHECK (outcome IN ('applied','rejected')),
  error text,
  revision bigint NOT NULL,
  modified_by text,
  applied_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS shopping_ops_list_idx ON grocery.shopping_ops (list_id, applied_at);

COMMIT;
