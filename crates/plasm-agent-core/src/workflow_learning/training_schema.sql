CREATE TABLE IF NOT EXISTS workflow_training_grants (
    id uuid PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    issuer text NOT NULL, grantee text NOT NULL, credential text NOT NULL, logical_session text,
    permissions jsonb NOT NULL, policy jsonb NOT NULL,
    revision bigint NOT NULL CHECK(revision > 0), revoked boolean NOT NULL DEFAULT false,
    expires_at timestamptz NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS workflow_training_grant_actor ON workflow_training_grants(tenant,project,grantee,credential);
CREATE TABLE IF NOT EXISTS workflow_training_grant_audit (
    sequence bigserial PRIMARY KEY, grant_id uuid NOT NULL REFERENCES workflow_training_grants(id),
    actor text NOT NULL, credential text NOT NULL, action text NOT NULL CHECK(action IN ('issued','revoked')),
    revision bigint NOT NULL, recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_training_sessions (
    tenant text NOT NULL, project text NOT NULL, logical_session text NOT NULL,
    revision bigint NOT NULL DEFAULT 0 CHECK(revision >= 0),
    state jsonb NOT NULL DEFAULT '{"state":"ordinary"}',
    active_epoch uuid, held boolean NOT NULL DEFAULT false,
    controller text NOT NULL, controller_credential text NOT NULL,
    purpose text NOT NULL DEFAULT 'operational' CHECK(purpose IN ('operational','evaluation')),
    PRIMARY KEY(tenant,project,logical_session)
);
ALTER TABLE workflow_training_sessions ADD COLUMN IF NOT EXISTS purpose text NOT NULL DEFAULT 'operational' CHECK(purpose IN ('operational','evaluation'));
CREATE TABLE IF NOT EXISTS workflow_training_epochs (
    id uuid PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    logical_session text NOT NULL, grant_id uuid NOT NULL REFERENCES workflow_training_grants(id),
    grant_revision bigint NOT NULL, start_revision bigint NOT NULL,
    closed boolean NOT NULL DEFAULT false, held boolean NOT NULL DEFAULT false, started_at timestamptz NOT NULL DEFAULT now(), closed_at timestamptz
);
CREATE TABLE IF NOT EXISTS workflow_training_requests (
    id uuid PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    logical_session text NOT NULL, revision bigint NOT NULL DEFAULT 1,
    requester text NOT NULL, credential text NOT NULL, purpose jsonb NOT NULL,
    state text NOT NULL CHECK(state IN ('pending','approved','declined','cancelled')),
    expires_at timestamptz NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_training_commands (
    tenant text NOT NULL, project text NOT NULL, logical_session text NOT NULL,
    actor text NOT NULL, credential text NOT NULL, idempotency_key uuid NOT NULL,
    command jsonb NOT NULL, receipt jsonb NOT NULL,
    PRIMARY KEY(tenant,project,logical_session,actor,credential,idempotency_key)
);
CREATE TABLE IF NOT EXISTS workflow_training_audit (
    sequence bigserial PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    logical_session text NOT NULL, actor text NOT NULL, credential text NOT NULL,
    action jsonb NOT NULL, receipt jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_training_releases (
    review_id uuid PRIMARY KEY REFERENCES workflow_learning_reviews(id),
    tenant text NOT NULL, project text NOT NULL, logical_session text NOT NULL,
    epoch_id uuid NOT NULL REFERENCES workflow_training_epochs(id),
    grant_id uuid NOT NULL REFERENCES workflow_training_grants(id), grant_revision bigint NOT NULL,
    actor text NOT NULL, revoked boolean NOT NULL DEFAULT false, released_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_training_run_releases (
    tenant text NOT NULL, project text NOT NULL, logical_session text NOT NULL, execution_id text NOT NULL,
    epoch_id uuid NOT NULL REFERENCES workflow_training_epochs(id),
    grant_id uuid NOT NULL REFERENCES workflow_training_grants(id), grant_revision bigint NOT NULL,
    actor text NOT NULL, revoked boolean NOT NULL DEFAULT false, released_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(tenant,project,logical_session,execution_id)
);
CREATE TABLE IF NOT EXISTS workflow_training_grant_commands (
    tenant text NOT NULL, project text NOT NULL, actor text NOT NULL, credential text NOT NULL,
    idempotency_key uuid NOT NULL, command jsonb NOT NULL, receipt jsonb NOT NULL,
    PRIMARY KEY(tenant,project,actor,credential,idempotency_key)
);
