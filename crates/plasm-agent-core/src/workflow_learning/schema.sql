CREATE TABLE IF NOT EXISTS workflow_learning_sessions (
    tenant text NOT NULL, logical_session text NOT NULL, project text NOT NULL,
    PRIMARY KEY(tenant,logical_session)
);
CREATE TABLE IF NOT EXISTS workflow_learning_receipts (
    tenant text NOT NULL, project text NOT NULL, logical_session text NOT NULL,
    session_revision bigint NOT NULL CHECK (session_revision >= 0), run_id text NOT NULL,
    receipt jsonb NOT NULL,
    PRIMARY KEY(tenant,project,logical_session,session_revision,run_id)
);
CREATE TABLE IF NOT EXISTS workflow_learning_execution_journal (
    tenant text NOT NULL, project text NOT NULL, logical_session text NOT NULL,
    execution_id text NOT NULL,
    state text NOT NULL CHECK(state IN ('pending','complete','unknown')),
    PRIMARY KEY(tenant,project,logical_session,execution_id)
);
CREATE TABLE IF NOT EXISTS workflow_learning_reviews (
    id uuid PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    logical_session text NOT NULL, session_revision bigint NOT NULL,
    digest text NOT NULL, seal jsonb NOT NULL, policy_version integer NOT NULL DEFAULT 1,
    state text NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','leased','completed','failed','awaiting_approval')),
    fence bigint NOT NULL DEFAULT 0, lease_until timestamptz,
    attempts integer NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    next_attempt timestamptz NOT NULL DEFAULT now(),
    failure_reason text, created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(tenant,project,logical_session,session_revision)
);
ALTER TABLE workflow_learning_reviews ADD COLUMN IF NOT EXISTS attempts integer NOT NULL DEFAULT 0 CHECK(attempts >= 0);
ALTER TABLE workflow_learning_reviews ADD COLUMN IF NOT EXISTS next_attempt timestamptz NOT NULL DEFAULT now();
ALTER TABLE workflow_learning_reviews DROP CONSTRAINT IF EXISTS workflow_learning_reviews_state_check;
ALTER TABLE workflow_learning_reviews ADD CONSTRAINT workflow_learning_reviews_state_check
    CHECK(state IN ('queued','leased','completed','failed','awaiting_approval'));
CREATE INDEX IF NOT EXISTS workflow_learning_jobs ON workflow_learning_reviews(state,lease_until,created_at);
CREATE INDEX IF NOT EXISTS workflow_learning_job_schedule ON workflow_learning_reviews(state,next_attempt,created_at);
CREATE TABLE IF NOT EXISTS workflow_learning_publication_approvals (
    review_id uuid PRIMARY KEY REFERENCES workflow_learning_reviews(id),
    tenant text NOT NULL, project text NOT NULL, actor text NOT NULL CHECK(length(trim(actor))>0),
    approved_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_learning_tenants (tenant text PRIMARY KEY);
CREATE TABLE IF NOT EXISTS workflow_learning_families (
    tenant text NOT NULL, project text NOT NULL, family text NOT NULL, variant text NOT NULL,
    version bigint NOT NULL DEFAULT 0,
    PRIMARY KEY(tenant,project,family,variant)
);
CREATE TABLE IF NOT EXISTS workflow_learning_examples (
    id uuid PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    family text NOT NULL, variant text NOT NULL, identity text NOT NULL,
    candidate jsonb NOT NULL, state text NOT NULL CHECK(state IN ('active','candidate','superseded','bad')),
    reason text, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS workflow_learning_active ON workflow_learning_examples(tenant,project,family,variant) WHERE state='active';
CREATE UNIQUE INDEX IF NOT EXISTS workflow_learning_active_identity ON workflow_learning_examples(tenant,project,family,variant,identity) WHERE state='active';
CREATE INDEX IF NOT EXISTS workflow_learning_intent_lexical ON workflow_learning_examples
USING gin(to_tsvector('english'::regconfig,
    coalesce(candidate->'intent'->>'purpose','') || ' ' ||
    coalesce((candidate->'intent'->'facets')::text,'') || ' ' ||
    coalesce((candidate->'intent'->'aliases')::text,'')));
CREATE TABLE IF NOT EXISTS workflow_learning_support (
    example_id uuid NOT NULL REFERENCES workflow_learning_examples(id),
    review_id uuid NOT NULL REFERENCES workflow_learning_reviews(id),
    PRIMARY KEY(example_id,review_id)
);
CREATE TABLE IF NOT EXISTS workflow_learning_proposal_batches (
    review_id uuid PRIMARY KEY REFERENCES workflow_learning_reviews(id),
    artifact_hashes jsonb NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_learning_proposals (
    review_id uuid NOT NULL REFERENCES workflow_learning_reviews(id),
    candidate_hash text NOT NULL,
    candidate jsonb NOT NULL,
    PRIMARY KEY(review_id,candidate_hash)
);
CREATE TABLE IF NOT EXISTS workflow_learning_frozen_publications (
    publication_order bigserial UNIQUE NOT NULL,
    review_id uuid PRIMARY KEY REFERENCES workflow_learning_reviews(id),
    tenant text NOT NULL, project text NOT NULL,
    actor text NOT NULL CHECK(length(trim(actor))>0),
    artifact_hashes jsonb NOT NULL,
    published_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_learning_normalization_calls (
    id uuid PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    review_id uuid REFERENCES workflow_learning_reviews(id), context jsonb NOT NULL,
    model text NOT NULL, endpoint text NOT NULL, request_hash text NOT NULL,
    request_body text NOT NULL CHECK(octet_length(request_body)<=131072),
    state text NOT NULL CHECK(state IN ('pending','accepted','rejected','transport_failed')),
    result jsonb, created_at timestamptz NOT NULL DEFAULT now(), finished_at timestamptz,
    CHECK((state='pending' AND result IS NULL AND finished_at IS NULL) OR
          (state<>'pending' AND result IS NOT NULL AND finished_at IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS workflow_learning_normalization_scope ON workflow_learning_normalization_calls(tenant,project,created_at,id);
CREATE TABLE IF NOT EXISTS workflow_learning_guidance_batches (
    review_id uuid PRIMARY KEY REFERENCES workflow_learning_reviews(id),
    associations jsonb NOT NULL CHECK(jsonb_typeof(associations)='array'),
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS workflow_learning_catalogue_guidance (
    id uuid PRIMARY KEY, tenant text NOT NULL, project text NOT NULL,
    family text NOT NULL, variant text NOT NULL, association jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(tenant,project,family,variant)
);
CREATE TABLE IF NOT EXISTS workflow_learning_guidance_support (
    association_id uuid NOT NULL REFERENCES workflow_learning_catalogue_guidance(id),
    review_id uuid NOT NULL REFERENCES workflow_learning_reviews(id),
    PRIMARY KEY(association_id,review_id)
);
CREATE INDEX IF NOT EXISTS workflow_learning_guidance_scope ON workflow_learning_catalogue_guidance(tenant,project);
-- Full teaching-format cutover: retain old JSON for repair, never reinterpret templates.
UPDATE workflow_learning_examples SET state='bad', reason='incompatible teaching format; retained for repair'
WHERE state IN ('active','candidate','superseded')
AND (coalesce(candidate#>>'{state,evidence,program,program,format}',candidate#>>'{state,program,program,format}')
    IS DISTINCT FROM 'whole_program_example'
    OR coalesce(candidate#>'{state,evidence,program,program,schema_version}',candidate#>'{state,program,program,schema_version}')
    IS DISTINCT FROM '3'::jsonb);
UPDATE workflow_learning_reviews SET state='failed',lease_until=NULL,
    failure_reason='incompatible teaching evidence version; retained for repair'
WHERE state IN ('queued','leased','awaiting_approval') AND (
    jsonb_typeof(seal->'executions') IS DISTINCT FROM 'array'
    OR EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(seal->'executions')='array' THEN seal->'executions' ELSE '[]'::jsonb END) receipt
        WHERE NOT (receipt ? 'observation' AND receipt ? 'catalogue_requirements')
        OR (receipt#>>'{observation,state}'='available' AND (
            receipt#>>'{observation,value,program,format}' IS DISTINCT FROM 'whole_program_example'
            OR receipt#>'{observation,value,program,schema_version}' IS DISTINCT FROM '3'::jsonb)))
);
