import type { KvArchiveIndexAdapter } from "./types.js";

function postgresUrl(): string {
  return (
    process.env.PLASM_ARCHIVE_POSTGRES_URL?.trim() ||
    process.env.PLASM_STATE_POSTGRES_URL?.trim() ||
    process.env.WORKFLOW_POSTGRES_URL?.trim() ||
    process.env.DATABASE_URL?.trim() ||
    ""
  );
}

const SCHEMA_SQL = `
CREATE TABLE IF NOT EXISTS plasm_agent_archive_index (
  index_key TEXT PRIMARY KEY,
  index_value TEXT NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
`;

type PgPool = {
  query(sql: string, params?: unknown[]): Promise<{ rows: Array<Record<string, unknown>> }>;
};

let sharedPool: PgPool | null = null;

async function pool(): Promise<PgPool> {
  if (sharedPool) return sharedPool;
  const url = postgresUrl();
  if (!url) {
    throw new Error("Postgres archive index requires DATABASE_URL or WORKFLOW_POSTGRES_URL");
  }
  const pg = await import("pg");
  const Pool = pg.default?.Pool ?? pg.Pool;
  sharedPool = new Pool({ connectionString: url }) as PgPool;
  await sharedPool.query(SCHEMA_SQL);
  return sharedPool;
}

export class PostgresArchiveIndexAdapter implements KvArchiveIndexAdapter {
  async set(key: string, value: string): Promise<void> {
    const db = await pool();
    await db.query(
      `INSERT INTO plasm_agent_archive_index (index_key, index_value, updated_at)
       VALUES ($1, $2, NOW())
       ON CONFLICT (index_key) DO UPDATE
       SET index_value = EXCLUDED.index_value, updated_at = NOW()`,
      [key, value],
    );
  }

  async get(key: string): Promise<string | null> {
    const db = await pool();
    const result = await db.query(
      `SELECT index_value FROM plasm_agent_archive_index WHERE index_key = $1`,
      [key],
    );
    const value = result.rows[0]?.index_value;
    return typeof value === "string" ? value : null;
  }

  async list(prefix: string): Promise<string[]> {
    const db = await pool();
    const result = await db.query(
      `SELECT index_key FROM plasm_agent_archive_index WHERE index_key LIKE $1`,
      [`${prefix}%`],
    );
    return result.rows
      .map((row) => row.index_key)
      .filter((key): key is string => typeof key === "string");
  }
}
