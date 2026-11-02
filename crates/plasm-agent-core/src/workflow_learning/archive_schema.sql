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
CREATE TABLE IF NOT EXISTS workflow_learning_archive_assessments (
 tenant text NOT NULL,project text NOT NULL,attempt_id text NOT NULL,assessment jsonb NOT NULL,
 PRIMARY KEY(tenant,project,attempt_id)
);
