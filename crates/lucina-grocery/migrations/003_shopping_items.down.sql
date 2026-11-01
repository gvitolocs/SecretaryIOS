-- Reverse of 003_shopping_items.sql. Restores the 002 shape.
-- Lost on the way down: custom item objects (their names in other languages,
-- section, icon and aliases). Rows keep raw_name (the text as typed) and
-- their catalog id; a row that pointed to a catalog object only through
-- item_id gets it back as catalog_item_id. Active rows that would share the
-- 002 key (list, coalesce(catalog_item_id, name_key)) are collapsed as 002
-- does: one is kept (needed first, then the newest), the rest are removed.
BEGIN;

UPDATE grocery.shopping_list_items
   SET catalog_item_id = item_id
 WHERE catalog_item_id IS NULL AND item_id IS NOT NULL AND item_id !~ '^u_';

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
DROP INDEX IF EXISTS grocery.shopping_list_items_one_active_per_item;
DROP INDEX IF EXISTS grocery.shopping_list_items_item_idx;
ALTER TABLE grocery.shopping_list_items DROP COLUMN IF EXISTS item_id;
DROP TABLE IF EXISTS grocery.shopping_custom_items;

COMMIT;
