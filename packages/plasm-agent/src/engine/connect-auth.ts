/**
 * Vercel Connect broker for outbound catalog HTTP (per-request tokens; never stored).
 */

export type ConnectTokenSubject =
  | { type: "app" }
  | { type: "user"; id: string };

export interface ConnectAuthOptions {
  connector?: string;
  subject?: ConnectTokenSubject;
  installationId?: string;
  scopes?: string[];
  vercelToken?: string;
}

/** Raised when Connect requires an end-user OAuth consent flow. */
export class ConnectAuthorizationRequiredError extends Error {
  readonly connector: string;
  readonly authorizationUrl?: string;

  constructor(message: string, connector: string, authorizationUrl?: string) {
    super(message);
    this.name = "ConnectAuthorizationRequiredError";
    this.connector = connector;
    this.authorizationUrl = authorizationUrl;
  }
}

function entryEnvSuffix(entryId: string): string {
  return entryId.toUpperCase().replace(/[^A-Z0-9]/g, "_");
}

/** Resolve connector UID for a catalog `entry_id` from env. */
export function connectorUidForEntry(entryId?: string): string | undefined {
  const trimmed = entryId?.trim();
  if (trimmed) {
    const perEntry =
      process.env[`PLASM_CONNECTOR_${entryEnvSuffix(trimmed)}`]?.trim() ??
      process.env[`CONNECTOR_${entryEnvSuffix(trimmed)}`]?.trim();
    if (perEntry) return perEntry;
  }
  return (
    process.env.PLASM_CONNECTOR_DEFAULT?.trim() ??
    process.env.CONNECTOR_DEFAULT?.trim()
  );
}

function defaultSubject(): ConnectTokenSubject {
  const type = process.env.PLASM_CONNECT_SUBJECT_TYPE?.trim().toLowerCase();
  if (type === "user") {
    const id = process.env.PLASM_CONNECT_USER_ID?.trim();
    if (!id) {
      throw new Error(
        "PLASM_CONNECT_SUBJECT_TYPE=user requires PLASM_CONNECT_USER_ID",
      );
    }
    return { type: "user", id };
  }
  return { type: "app" };
}

function parseScopes(raw?: string): string[] | undefined {
  const value = raw?.trim();
  if (!value) return undefined;
  const parts = value.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
  return parts.length ? parts : undefined;
}

export function connectAuthOptionsForEntry(entryId?: string): ConnectAuthOptions | undefined {
  const connector = connectorUidForEntry(entryId);
  if (!connector) return undefined;
  return {
    connector,
    subject: defaultSubject(),
    installationId: process.env.PLASM_CONNECT_INSTALLATION_ID?.trim() || undefined,
    scopes: parseScopes(process.env.PLASM_CONNECT_SCOPES),
    vercelToken: process.env.VERCEL_OIDC_TOKEN?.trim() || undefined,
  };
}

type ConnectModule = typeof import("@vercel/connect");

async function loadConnect(): Promise<ConnectModule> {
  return import("@vercel/connect");
}

function isUserAuthRequired(err: unknown): err is { authorizationUrl?: string } {
  return (
    err != null &&
    typeof err === "object" &&
    (err as { name?: string }).name === "UserAuthorizationRequiredError"
  );
}

/** Mint a short-lived Connect access token for `entryId`, or undefined when not configured. */
export async function resolveConnectBearer(
  entryId?: string,
  overrides?: Partial<ConnectAuthOptions>,
): Promise<string | undefined> {
  const base = connectAuthOptionsForEntry(entryId);
  if (!base?.connector) return undefined;

  const connector = overrides?.connector ?? base.connector;
  const subject = overrides?.subject ?? base.subject ?? { type: "app" as const };
  const installationId = overrides?.installationId ?? base.installationId;
  const scopes = overrides?.scopes ?? base.scopes;
  const vercelToken = overrides?.vercelToken ?? base.vercelToken;

  try {
    const { getToken } = await loadConnect();
    return await getToken(
      connector,
      {
        subject,
        ...(installationId ? { installationId } : {}),
        ...(scopes ? { scopes } : {}),
      },
      vercelToken ? { vercelToken } : undefined,
    );
  } catch (err: unknown) {
    if (isUserAuthRequired(err)) {
      const url =
        typeof (err as { authorizationUrl?: string }).authorizationUrl === "string"
          ? (err as { authorizationUrl: string }).authorizationUrl
          : undefined;
      throw new ConnectAuthorizationRequiredError(
        `Vercel Connect authorization required for connector ${connector}`,
        connector,
        url,
      );
    }
    throw err;
  }
}
