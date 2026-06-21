import type { IncomingMessage, ServerResponse } from "node:http";

import type { AuthoringContext } from "./context.js";

export const PLASM_CHANNEL_KIND = "channel" as const;

export type HttpMethod =
  | "GET"
  | "POST"
  | "PUT"
  | "PATCH"
  | "DELETE"
  | "HEAD"
  | "OPTIONS";

export type ChannelHandler = (
  req: IncomingMessage,
  res: ServerResponse,
  ctx: AuthoringContext,
) => void | Promise<void>;

export interface ChannelRoute {
  method: HttpMethod;
  path: string;
  handler: ChannelHandler;
}

export interface ChannelDefinition {
  readonly __plasmSlotKind: typeof PLASM_CHANNEL_KIND;
  name: string;
  routes: ChannelRoute[];
}

export interface DefineChannelInput {
  name: string;
  routes: ChannelRoute[];
}

const RESERVED_PATH_PREFIXES = ["/plasm/", "/operator"] as const;

export function validateChannelRoute(route: ChannelRoute): string | null {
  if (!route.path.startsWith("/")) {
    return `channel route path must start with '/': ${route.path}`;
  }
  for (const prefix of RESERVED_PATH_PREFIXES) {
    if (route.path === prefix.replace(/\/$/, "") || route.path.startsWith(prefix)) {
      return `channel route conflicts with reserved prefix ${prefix}: ${route.path}`;
    }
  }
  return null;
}

/** Nitro-shaped HTTP channel (deterministic ingress; call stubs, not plasm_context). */
export function defineChannel(input: DefineChannelInput): ChannelDefinition {
  if (!input.name?.trim()) {
    throw new Error("defineChannel: name is required");
  }
  if (!input.routes?.length) {
    throw new Error("defineChannel: at least one route is required");
  }
  const routes = input.routes.map((route) => ({
    ...route,
    method: route.method.toUpperCase() as HttpMethod,
    path: route.path.trim(),
  }));
  for (const route of routes) {
    const err = validateChannelRoute(route);
    if (err) throw new Error(`defineChannel(${input.name}): ${err}`);
  }
  return Object.freeze({
    __plasmSlotKind: PLASM_CHANNEL_KIND,
    name: input.name.trim(),
    routes,
  });
}

export function isChannelDefinition(value: unknown): value is ChannelDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as ChannelDefinition).__plasmSlotKind === PLASM_CHANNEL_KIND
  );
}
