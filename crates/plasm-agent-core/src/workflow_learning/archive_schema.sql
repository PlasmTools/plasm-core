CREATE TABLE IF NOT EXISTS workflow_learning_archives (
 tenant text NOT NULL,project text NOT NULL,digest text NOT NULL,document jsonb NOT NULL,
 PRIMARY KEY(tenant,project,digest)
);
CREATE TABLE IF NOT EXISTS workflow_learning_archive_items (
 tenant text NOT NULL,project text NOT NULL,archive_digest text NOT NULL,item_digest text NOT NULL,
 identity text NOT NULL,source_scope jsonb NOT NULL,record jsonb NOT NULL,disposition jsonb NOT NULL,
 PRIMARY KEY(tenant,project,archive_digest,item_digest),
 FOREIGN KEY(tenant,project,archive_digest) REFERENCES workflow_learning_archives(tenant,project,digest)
);
CREATE TABLE IF NOT EXISTS workflow_learning_archive_example_support (
 example_id uuid NOT NULL REFERENCES workflow_learning_examples(id),
 tenant text NOT NULL,project text NOT NULL,archive_digest text NOT NULL,item_digest text NOT NULL,
 PRIMARY KEY(example_id,tenant,project,archive_digest,item_digest),
 FOREIGN KEY(tenant,project,archive_digest,item_digest) REFERENCES workflow_learning_archive_items(tenant,project,archive_digest,item_digest)
);
CREATE TABLE IF NOT EXISTS workflow_learning_archive_guidance_support (
 association_id uuid NOT NULL REFERENCES workflow_learning_catalogue_guidance(id),
 tenant text NOT NULL,project text NOT NULL,archive_digest text NOT NULL,item_digest text NOT NULL,
 PRIMARY KEY(association_id,tenant,project,archive_digest,item_digest),
 FOREIGN KEY(tenant,project,archive_digest,item_digest) REFERENCES workflow_learning_archive_items(tenant,project,archive_digest,item_digest)
);
-- A separate, explicit trusted decision may attest a particular imported review.
-- It never follows automatically from accepting an archive or its self-asserted
-- historical digests.
CREATE TABLE IF NOT EXISTS workflow_training_archive_review_decisions (
 tenant text NOT NULL, project text NOT NULL, archive_digest text NOT NULL,
 source_tenant text NOT NULL, source_project text NOT NULL, review_id uuid NOT NULL,
 seal_digest text NOT NULL, actor text NOT NULL, credential text NOT NULL, authority jsonb NOT NULL,
 expires_at timestamptz NOT NULL, decision_id uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant,project,archive_digest,source_tenant,source_project,review_id,seal_digest),
 UNIQUE(tenant,project,decision_id)
);
