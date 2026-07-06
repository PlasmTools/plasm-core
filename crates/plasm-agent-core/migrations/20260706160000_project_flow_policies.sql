-- Project-scoped plan flow policy (draft + published revisions).

CREATE TABLE IF NOT EXISTS project_flow_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    workspace_slug TEXT NOT NULL,
    project_slug TEXT NOT NULL,
    published_revision BIGINT NOT NULL DEFAULT 0,
    published_policy_json JSONB,
    published_at TIMESTAMPTZ,
    published_by_subject TEXT,
    draft_policy_json JSONB,
    draft_updated_at TIMESTAMPTZ,
    draft_validated_at TIMESTAMPTZ,
    draft_validation_ok BOOLEAN,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, workspace_slug, project_slug)
);

CREATE INDEX IF NOT EXISTS project_flow_policies_scope_idx
    ON project_flow_policies (tenant_id, workspace_slug, project_slug);
