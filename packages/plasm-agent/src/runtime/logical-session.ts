import { createHash, randomUUID } from "node:crypto";

const WIRE_PREFIX = "l_";
const TOKEN_LEN = 22;

/** Canonical MCP logical session wire ref: `l_<base64url-unpadded-uuid-bytes>`. */
export function formatLogicalSessionWireRef(uuidBytes: Buffer): string {
  if (uuidBytes.length !== 16) {
    throw new Error("logical session UUID must be 16 bytes");
  }
  const token = uuidBytes.toString("base64url");
  if (token.length !== TOKEN_LEN) {
    throw new Error(`unexpected wire token length ${token.length}`);
  }
  return `${WIRE_PREFIX}${token}`;
}

export function parseLogicalSessionWireRef(ref: string): Buffer {
  const trimmed = ref.trim();
  if (!trimmed.startsWith(WIRE_PREFIX)) {
    throw new Error(`invalid logical_session_ref: ${ref}`);
  }
  const token = trimmed.slice(WIRE_PREFIX.length);
  if (token.length !== TOKEN_LEN) {
    throw new Error(`invalid logical_session_ref token length: ${ref}`);
  }
  const bytes = Buffer.from(token, "base64url");
  if (bytes.length !== 16) {
    throw new Error(`invalid logical_session_ref payload: ${ref}`);
  }
  return bytes;
}

/**
 * Idempotent logical session identity for `(tenantScope, intent)`.
 * Persisted via SessionStore on first `plasm_context` open.
 */
export function mintLogicalSessionId(tenantScope: string, intent: string): {
  logicalSessionId: string;
  logicalSessionRef: string;
} {
  const digest = createHash("sha256")
    .update(`${tenantScope}\0${intent}`, "utf8")
    .digest();
  const bytes = Buffer.alloc(16);
  digest.copy(bytes, 0, 0, 16);
  bytes[6] = (bytes[6]! & 0x0f) | 0x50;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const logicalSessionId = [
    bytes.toString("hex", 0, 4),
    bytes.toString("hex", 4, 6),
    bytes.toString("hex", 6, 8),
    bytes.toString("hex", 8, 10),
    bytes.toString("hex", 10, 16),
  ].join("-");
  return {
    logicalSessionId,
    logicalSessionRef: formatLogicalSessionWireRef(bytes),
  };
}

export function newEphemeralLogicalSession(): {
  logicalSessionId: string;
  logicalSessionRef: string;
} {
  const id = randomUUID();
  const bytes = Buffer.from(id.replace(/-/g, ""), "hex");
  return {
    logicalSessionId: id,
    logicalSessionRef: formatLogicalSessionWireRef(bytes),
  };
}
