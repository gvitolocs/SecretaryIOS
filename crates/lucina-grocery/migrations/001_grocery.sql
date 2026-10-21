-- Grocery Intelligence lives in a separate schema inside Hermes' existing PostgreSQL instance.
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE SCHEMA IF NOT EXISTS grocery;

CREATE TABLE IF NOT EXISTS grocery.canonical_products (
  id text PRIMARY KEY,
  display_name text NOT NULL,
  category text NOT NULL DEFAULT 'other',
  aliases jsonb NOT NULL DEFAULT '[]'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS grocery.stores (
  id text PRIMARY KEY,
  chain_slug text NOT NULL,
  display_name text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS grocery.store_context (
  user_id text PRIMARY KEY,
  store_id text REFERENCES grocery.stores(id),
  confidence numeric(4,3) NOT NULL,
  source text NOT NULL,
  observed_at timestamptz NOT NULL DEFAULT now(),
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS grocery.catalog_products (
  id text PRIMARY KEY,
  canonical_product_id text REFERENCES grocery.canonical_products(id),
  store_id text REFERENCES grocery.stores(id),
  external_id text,
  name text NOT NULL,
  brand text,
  category text,
  quantity numeric,
  unit text,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  UNIQUE (store_id, external_id)
);

CREATE TABLE IF NOT EXISTS grocery.price_observations (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  product_id text REFERENCES grocery.canonical_products(id),
  catalog_product_id text REFERENCES grocery.catalog_products(id),
  store_id text REFERENCES grocery.stores(id),
  price_dkk numeric(12,2) NOT NULL,
  normal_price_dkk numeric(12,2),
  offer_price_dkk numeric(12,2),
  observed_at timestamptz NOT NULL DEFAULT now(),
  valid_from timestamptz,
  valid_to timestamptz,
  source text NOT NULL,
  source_url text,
  verification_status text NOT NULL CHECK (verification_status IN ('receipt_verified','catalog_verified','offer_verified','historical_snapshot','catalog_inferred','estimated')),
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS grocery.grocery_transactions (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id text NOT NULL,
  store_id text REFERENCES grocery.stores(id),
  purchased_at timestamptz NOT NULL,
  recorded_at timestamptz NOT NULL DEFAULT now(),
  source text NOT NULL,
  verification_status text NOT NULL CHECK (verification_status IN ('receipt_verified','catalog_verified','offer_verified','historical_snapshot','catalog_inferred','estimated')),
  confidence numeric(4,3) NOT NULL,
  total_dkk numeric(12,2) NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS grocery.grocery_transaction_items (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  transaction_id uuid NOT NULL REFERENCES grocery.grocery_transactions(id) ON DELETE CASCADE,
  canonical_product_id text REFERENCES grocery.canonical_products(id),
  raw_product_name text NOT NULL,
  catalog_product_id text REFERENCES grocery.catalog_products(id),
  quantity numeric,
  unit text,
  unit_price_dkk numeric(12,2) NOT NULL,
  total_price_dkk numeric(12,2) NOT NULL,
  category text,
  brand text,
  match_confidence numeric(4,3) NOT NULL,
  price_source text NOT NULL,
  verification_status text NOT NULL CHECK (verification_status IN ('receipt_verified','catalog_verified','offer_verified','historical_snapshot','catalog_inferred','estimated')),
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS grocery.pantry_events (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id text NOT NULL,
  product_id text REFERENCES grocery.canonical_products(id),
  event_type text NOT NULL CHECK (event_type IN ('purchased','consumed','finished','discarded','adjusted','expired')),
  quantity numeric,
  unit text,
  occurred_at timestamptz NOT NULL DEFAULT now(),
  source text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS grocery.shopping_list_items (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id text NOT NULL,
  product_id text REFERENCES grocery.canonical_products(id),
  raw_name text NOT NULL,
  status text NOT NULL DEFAULT 'needed' CHECK (status IN ('needed','purchased','removed')),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS grocery.transaction_reconciliations (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  transaction_id uuid NOT NULL REFERENCES grocery.grocery_transactions(id) ON DELETE CASCADE,
  previous_total_dkk numeric(12,2) NOT NULL,
  previous_status text NOT NULL,
  reconciled_at timestamptz NOT NULL DEFAULT now(),
  source text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS grocery.webhook_deliveries (
  delivery_id text PRIMARY KEY,
  provider text NOT NULL,
  received_at timestamptz NOT NULL DEFAULT now(),
  occurred_at timestamptz,
  event_type text NOT NULL,
  payload jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS grocery_transactions_user_purchased_idx ON grocery.grocery_transactions(user_id, purchased_at DESC);
CREATE INDEX IF NOT EXISTS grocery_transactions_store_purchased_idx ON grocery.grocery_transactions(store_id, purchased_at DESC);
CREATE INDEX IF NOT EXISTS grocery_items_product_idx ON grocery.grocery_transaction_items(canonical_product_id);
CREATE INDEX IF NOT EXISTS grocery_price_observations_lookup_idx ON grocery.price_observations(product_id, store_id, observed_at DESC);
CREATE INDEX IF NOT EXISTS grocery_pantry_events_user_product_idx ON grocery.pantry_events(user_id, product_id, occurred_at DESC);
