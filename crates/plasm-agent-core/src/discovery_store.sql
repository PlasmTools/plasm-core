CREATE EXTENSION IF NOT EXISTS vector;
CREATE TABLE IF NOT EXISTS discovery_revisions (
    revision_id text PRIMARY KEY,
    entry_id text NOT NULL,
    manifest jsonb NOT NULL,
    cgs bytea NOT NULL,
    profile jsonb NOT NULL
);
CREATE TABLE IF NOT EXISTS discovery_capabilities (
    revision_id text NOT NULL REFERENCES discovery_revisions(revision_id),
    capability text NOT NULL,
    entity text NOT NULL,
    document jsonb NOT NULL,
    search tsvector NOT NULL,
    embedding vector(1536) NOT NULL,
    PRIMARY KEY (revision_id, capability)
);
CREATE TABLE IF NOT EXISTS discovery_compiled_recipes (
    revision_id text PRIMARY KEY REFERENCES discovery_revisions(revision_id) ON DELETE CASCADE,
    recipes bytea NOT NULL
);
CREATE INDEX IF NOT EXISTS discovery_capabilities_search ON discovery_capabilities USING gin(search);
CREATE TABLE IF NOT EXISTS discovery_generations (
    generation_id text PRIMARY KEY,
    bindings jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS discovery_generation_catalogs (
    generation_id text NOT NULL REFERENCES discovery_generations(generation_id) ON DELETE CASCADE,
    entry_id text NOT NULL,
    revision_id text NOT NULL REFERENCES discovery_revisions(revision_id),
    PRIMARY KEY (generation_id, entry_id)
);
CREATE TABLE IF NOT EXISTS discovery_active (
    deployment_id text PRIMARY KEY,
    generation_id text NOT NULL REFERENCES discovery_generations(generation_id)
);
CREATE TABLE IF NOT EXISTS discovery_session_pins (
    session_id text PRIMARY KEY,
    generation_id text NOT NULL REFERENCES discovery_generations(generation_id),
    expires_at timestamptz NOT NULL
);
CREATE TABLE IF NOT EXISTS discovery_intent_embeddings (
    cache_key text PRIMARY KEY,
    profile jsonb NOT NULL,
    embedding vector(1536) NOT NULL
);
CREATE TABLE IF NOT EXISTS discovery_selector_cache (
    cache_key text PRIMARY KEY,
    envelope text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS discovery_routing_receipts (
    routing_ref text PRIMARY KEY,
    generation_id text NOT NULL REFERENCES discovery_generations(generation_id),
    scope_key text NOT NULL,
    logical_session text,
    allowed_catalogs jsonb NOT NULL,
    receipt jsonb NOT NULL,
    chosen_alternative integer,
    resolution jsonb,
    expires_at timestamptz NOT NULL
);
