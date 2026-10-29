-- Lucina's proactive engine (crates/lucina-proactive). Schema lucina_proactive
-- in the grocery Postgres. Applied by `lucina-proactive migrate`, which keeps
-- lucina_proactive.schema_migrations.

-- Signals as observed: one row per distinct dedup key (the same thing seen
-- again on the next tick is not considered again).
CREATE TABLE lucina_proactive.events (
    id uuid PRIMARY KEY,
    source text NOT NULL,              -- calendar | reminders | notes | timeline | health (MVP4: email | github | pokoin)
    kind text NOT NULL,                -- meeting_prep | reminder_overdue | due_no_plan | ...
    dedup_key text NOT NULL UNIQUE,
    subject text NOT NULL DEFAULT '',  -- one private line (decision log, Pulse)
    signal text NOT NULL DEFAULT '',   -- why the source thinks it matters
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    observed_at timestamptz NOT NULL,
    expires_at timestamptz
);
CREATE INDEX events_observed_idx ON lucina_proactive.events (observed_at DESC);
CREATE INDEX events_kind_idx ON lucina_proactive.events (kind, observed_at DESC);

-- One scored, decided candidate per event. A held candidate is updated in
-- place each time the gate looks at it again.
CREATE TABLE lucina_proactive.candidates (
    id uuid PRIMARY KEY,
    event_id uuid NOT NULL UNIQUE REFERENCES lucina_proactive.events (id) ON DELETE CASCADE,
    kind text NOT NULL,
    urgency real NOT NULL,
    relevance real NOT NULL,
    confidence real NOT NULL,
    actionability real NOT NULL,
    score real NOT NULL,
    decision text NOT NULL CHECK (decision IN ('drop', 'digest', 'hold', 'sent', 'pulsed')),
    reason text NOT NULL,
    priority text NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high')),
    created_at timestamptz NOT NULL,
    decided_at timestamptz NOT NULL,
    hold_until timestamptz,
    expires_at timestamptz,
    pulse_id text                       -- the Pulse outreach that surfaced it
);
CREATE INDEX candidates_decision_idx ON lucina_proactive.candidates (decision, decided_at DESC);

-- What Lucina sent (or, dry-run, would have sent).
CREATE TABLE lucina_proactive.outreach (
    id text PRIMARY KEY,                -- po_<ULID>; the deep link is /?outreach=<id>
    candidate_id uuid REFERENCES lucina_proactive.candidates (id) ON DELETE SET NULL,
    kind text NOT NULL,
    channel text NOT NULL,              -- app | telegram | app+telegram | none
    message text NOT NULL,
    priority text NOT NULL CHECK (priority IN ('low', 'normal', 'high')),
    actions jsonb NOT NULL DEFAULT '[]'::jsonb,
    why text NOT NULL,
    language text NOT NULL,
    dry_run boolean NOT NULL,
    status text NOT NULL CHECK (status IN ('dry_run', 'sent', 'partial', 'failed')),
    result jsonb NOT NULL DEFAULT '{}'::jsonb,   -- per channel: notify answer, Telegram message id, errors
    sent_at timestamptz NOT NULL,
    timeline_id text,
    unsolicited boolean NOT NULL DEFAULT true    -- false for the Daily Pulse (outside the daily budget)
);
CREATE INDEX outreach_sent_idx ON lucina_proactive.outreach (sent_at DESC);
CREATE INDEX outreach_kind_idx ON lucina_proactive.outreach (kind, sent_at DESC);

-- "Utile / Non ora / Basta così". Stored now; MVP 5 learns from it.
CREATE TABLE lucina_proactive.feedback (
    id uuid PRIMARY KEY,
    outreach_id text NOT NULL REFERENCES lucina_proactive.outreach (id) ON DELETE CASCADE,
    action text NOT NULL CHECK (action IN ('useful', 'not_now', 'stop')),
    kind text NOT NULL,
    source text NOT NULL DEFAULT 'app',  -- app | telegram | api
    external_id text UNIQUE,             -- the server's id for the tap (idempotent pulls)
    created_at timestamptz NOT NULL
);
CREATE INDEX feedback_kind_idx ON lucina_proactive.feedback (kind, action, created_at DESC);

-- Small per-source state: health hysteresis, cursors, the judge's cache,
-- the last Pulse.
CREATE TABLE lucina_proactive.source_state (
    source text NOT NULL,
    key text NOT NULL,
    state jsonb NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (source, key)
);

-- MVP 3 (schema only, empty): background tasks that survive the app being
-- closed and report when done, blocked or needing a decision.
CREATE TABLE lucina_proactive.tasks (
    id uuid PRIMARY KEY,
    title text NOT NULL,
    kind text NOT NULL,
    status text NOT NULL CHECK (status IN ('queued', 'running', 'blocked', 'needs_decision', 'done', 'failed', 'cancelled')),
    spec jsonb NOT NULL DEFAULT '{}'::jsonb,
    progress jsonb NOT NULL DEFAULT '{}'::jsonb,
    result jsonb,
    created_by text NOT NULL DEFAULT 'owner',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    next_run_at timestamptz,
    reported_at timestamptz,
    outreach_id text REFERENCES lucina_proactive.outreach (id) ON DELETE SET NULL
);
CREATE INDEX tasks_status_idx ON lucina_proactive.tasks (status, next_run_at);

-- MVP 3 (schema only, empty): Smart Approval. Approve / Edit / Ignore before
-- sending email, spending, deleting or other high-impact changes.
CREATE TABLE lucina_proactive.approvals (
    id uuid PRIMARY KEY,
    task_id uuid REFERENCES lucina_proactive.tasks (id) ON DELETE CASCADE,
    action_kind text NOT NULL CHECK (action_kind IN ('send_email', 'spend', 'delete', 'high_impact', 'other')),
    summary text NOT NULL,
    draft jsonb NOT NULL DEFAULT '{}'::jsonb,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'edited', 'ignored', 'expired')),
    decided_draft jsonb,
    outreach_id text REFERENCES lucina_proactive.outreach (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    decided_at timestamptz,
    expires_at timestamptz
);
CREATE INDEX approvals_status_idx ON lucina_proactive.approvals (status, created_at DESC);

-- MVP 5: automations Lucina proposes herself ("you check your server often;
-- shall I monitor it and only tell you when it gets worse?").
CREATE TABLE lucina_proactive.automation_suggestions (
    id uuid PRIMARY KEY,
    pattern text NOT NULL,
    evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    proposal text NOT NULL,
    status text NOT NULL DEFAULT 'proposed' CHECK (status IN ('proposed', 'accepted', 'declined', 'active', 'retired')),
    dedup_key text UNIQUE,
    outreach_id text REFERENCES lucina_proactive.outreach (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    decided_at timestamptz
);
