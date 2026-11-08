-- Client row ids (SecretaryIOS docs/verification.md, S2; docs/shopping-v2.md).
-- Additive and idempotent: safe to run more than once. Reverse with
-- migrations/004_shopping_row_aliases.down.sql.
--
-- An add op carries the client's optimistic row id (`rowId`, or a UUID in
-- `itemId` from the first v2 clients). A new row takes that id. When the add
-- merges into the object's existing active row instead, the client's id is
-- recorded here as an alias of that row, so the client's later ops on its
-- optimistic row (same batch, or a retry after a lost ack) still land
-- instead of being rejected `item_not_found` and dropped.
BEGIN;

CREATE TABLE IF NOT EXISTS grocery.shopping_row_aliases (
  alias_id uuid PRIMARY KEY,
  list_id uuid NOT NULL,
  item_id uuid NOT NULL REFERENCES grocery.shopping_list_items(id) ON DELETE CASCADE,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS shopping_row_aliases_item_idx ON grocery.shopping_row_aliases (item_id);

COMMIT;
