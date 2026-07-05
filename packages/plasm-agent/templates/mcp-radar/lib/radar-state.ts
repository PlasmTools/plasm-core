function isVercelRuntime(): boolean {
  return (
    process.env.VERCEL === "1" ||
    Boolean(process.env.VERCEL_DEPLOYMENT_ID?.trim()) ||
    Boolean(process.env.VERCEL_ENV?.trim())
  );
}

/** Host infra — not a substitute for Plasm catalog calls. */
export function gatewayConfigured(): boolean {
  if (
    process.env.AI_GATEWAY_API_KEY?.trim() ||
    process.env.AI_API_GATEWAY_KEY?.trim() ||
    process.env.AI_GATEWAY_KEY?.trim()
  ) {
    return true;
  }
  return isVercelRuntime();
}

/** Outbound Tavily auth present on host — agent still calls Tavily via Plasm. */
export function tavilyConfigured(): boolean {
  return Boolean(process.env.TAVILY_API_TOKEN?.trim());
}
