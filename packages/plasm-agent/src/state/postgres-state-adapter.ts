import type { SymbolRegistrySnapshot } from "../symbol-registry.js";
import { decodeSessionState, decodeListedSession, encodeSessionState, type AgentSessionState } from "../session-state.js";
import { sessionStorageKey, type LogicalSessionRef } from "../runtime/session-contract.js";
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

export class PostgresStateAdapter implements AgentStateStore {
  constructor(private readonly tenantScope: string, private readonly connection?: PgPool) { }

  private database(): Promise<PgPool> {
    return this.connection ? Promise.resolve(this.connection) : pool();
  }

  backend(): StateBackend {
    return "postgres";
  }

  async get(ref: LogicalSessionRef): Promise<AgentSessionState | null> {
    const db = await this.database();
    const result = await db.query(
      `SELECT payload FROM plasm_agent_state
       WHERE tenant_id = $1 AND kind = 'session-v2' AND state_key = $2`,
      [this.tenantScope, sessionStorageKey({ tenantScope: this.tenantScope, logicalSessionRef: ref })],
    );
    const row = result.rows[0]?.payload;
    return row === undefined ? null : decodeSessionState(row, { tenantScope: this.tenantScope, logicalSessionRef: ref });
  }

  async put(state: AgentSessionState): Promise<void> {
    const encoded = encodeSessionState(state, { tenantScope: this.tenantScope, logicalSessionRef: state.logicalSessionRef });
    const db = await this.database();
    await db.query(
      `INSERT INTO plasm_agent_state (tenant_id, kind, state_key, payload, updated_at)
       VALUES ($1, 'session-v2', $2, $3::jsonb, NOW())
       ON CONFLICT (tenant_id, kind, state_key)
       DO UPDATE SET payload = EXCLUDED.payload, updated_at = NOW()`,
      [this.tenantScope, sessionStorageKey({ tenantScope: this.tenantScope, logicalSessionRef: state.logicalSessionRef }), encoded],
    );
  }

  async listSessions(): Promise<AgentSessionState[]> {
    const db = await this.database();
    const result = await db.query(
      `SELECT state_key, payload FROM plasm_agent_state
       WHERE tenant_id = $1 AND kind = 'session-v2'`,
      [this.tenantScope],
    );
    return result.rows.map((row) => {
      const state = decodeListedSession(row.payload, this.tenantScope);
      if (row.state_key !== sessionStorageKey({ tenantScope: this.tenantScope, logicalSessionRef: state.logicalSessionRef })) throw new Error("Session key does not match its identity");
      return state;
    });
  }

  async getSymbolRegistry(tenantId: string): Promise<SymbolRegistrySnapshot | null> {
    const db = await this.database();
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
    const db = await this.database();
    await db.query(
      `INSERT INTO plasm_agent_state (tenant_id, kind, state_key, payload, updated_at)
       VALUES ($1, 'symbols', 'registry', $2::jsonb, NOW())
       ON CONFLICT (tenant_id, kind, state_key)
       DO UPDATE SET payload = EXCLUDED.payload, updated_at = NOW()`,
      [tenantId, JSON.stringify(snapshot)],
    );
  }
}
