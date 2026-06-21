import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import type { AgentSessionState } from "../session-state.js";
import type { AgentStateStore, StateBackend } from "./define-state.js";

function postgresUrl(): string {
  return (
    process.env.PLASM_STATE_POSTGRES_URL?.trim() ||
    process.env.WORKFLOW_POSTGRES_URL?.trim() ||
    process.env.DATABASE_URL?.trim() ||
    ""
  );
}

const SCHEMA_SQL = `
CREATE TABLE IF NOT EXISTS plasm_agent_state (
  tenant_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  state_key TEXT NOT NULL,
  payload JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (tenant_id, kind, state_key)
);
`;

type PgPool = {
  query(sql: string, params?: unknown[]): Promise<{ rows: Array<Record<string, unknown>> }>;
  end(): Promise<void>;
};

let sharedPool: PgPool | null = null;

async function pool(): Promise<PgPool> {
  if (sharedPool) return sharedPool;
  const url = postgresUrl();
  if (!url) {
    throw new Error(
      "Postgres state backend requires PLASM_STATE_POSTGRES_URL, WORKFLOW_POSTGRES_URL, or DATABASE_URL",
    );
  }
  const pg = await import("pg");
  const Pool = pg.default?.Pool ?? pg.Pool;
  sharedPool = new Pool({ connectionString: url }) as PgPool;
  await sharedPool.query(SCHEMA_SQL);
  return sharedPool;
}

function intentKey(intent: string): string {
  return Buffer.from(intent, "utf8").toString("base64url");
}

export class PostgresStateAdapter implements AgentStateStore {
  constructor(private readonly tenantScope: string) {}

  backend(): StateBackend {
    return "postgres";
  }

  async get(intent: string): Promise<AgentSessionState | null> {
    const db = await pool();
    const result = await db.query(
      `SELECT payload FROM plasm_agent_state
       WHERE tenant_id = $1 AND kind = 'session' AND state_key = $2`,
      [this.tenantScope, intentKey(intent)],
    );
    const row = result.rows[0]?.payload;
    return row ? (row as AgentSessionState) : null;
  }

  async put(state: AgentSessionState): Promise<void> {
    const db = await pool();
    await db.query(
      `INSERT INTO plasm_agent_state (tenant_id, kind, state_key, payload, updated_at)
       VALUES ($1, 'session', $2, $3::jsonb, NOW())
       ON CONFLICT (tenant_id, kind, state_key)
       DO UPDATE SET payload = EXCLUDED.payload, updated_at = NOW()`,
      [this.tenantScope, intentKey(state.intent), JSON.stringify(state)],
    );
  }

  async listIntents(): Promise<string[]> {
    const db = await pool();
    const result = await db.query(
      `SELECT payload FROM plasm_agent_state
       WHERE tenant_id = $1 AND kind = 'session'`,
      [this.tenantScope],
    );
    return result.rows
      .map((row) => (row.payload as AgentSessionState | undefined)?.intent)
      .filter((intent): intent is string => Boolean(intent));
  }

  async getSymbolRegistry(tenantId: string): Promise<SymbolRegistrySnapshot | null> {
    const db = await pool();
    const result = await db.query(
      `SELECT payload FROM plasm_agent_state
       WHERE tenant_id = $1 AND kind = 'symbols' AND state_key = 'registry'`,
      [tenantId],
    );
    const row = result.rows[0]?.payload;
    return row ? (row as SymbolRegistrySnapshot) : null;
  }

  async putSymbolRegistry(
    tenantId: string,
    snapshot: SymbolRegistrySnapshot,
  ): Promise<void> {
    const db = await pool();
    await db.query(
      `INSERT INTO plasm_agent_state (tenant_id, kind, state_key, payload, updated_at)
       VALUES ($1, 'symbols', 'registry', $2::jsonb, NOW())
       ON CONFLICT (tenant_id, kind, state_key)
       DO UPDATE SET payload = EXCLUDED.payload, updated_at = NOW()`,
      [tenantId, JSON.stringify(snapshot)],
    );
  }
}
