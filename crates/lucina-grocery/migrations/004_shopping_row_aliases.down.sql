-- Reverse of 004_shopping_row_aliases.sql. Ops naming an alias are then
-- rejected `item_not_found` again (the S2 bug); nothing else changes.
DROP TABLE IF EXISTS grocery.shopping_row_aliases;
