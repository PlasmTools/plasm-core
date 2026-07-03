/** Direct Algolia HN search for MCP radar preflight (no Plasm stub / engine required). */

const ALGOLIA_SEARCH_BY_DATE = "https://hn.algolia.com/api/v1/search_by_date";

export interface AlgoliaHit {
  objectID: string;
  title?: string;
  url?: string;
}

export interface HnAlgoliaStoryRow {
  id: string;
  title?: string;
  url?: string;
}

export async function fetchHnMcpStoriesByDate(options: {
  query: string;
  tags?: string;
  perPage?: number;
}): Promise<HnAlgoliaStoryRow[]> {
  const params = new URLSearchParams({
    query: options.query,
    tags: options.tags ?? "story",
    hitsPerPage: String(options.perPage ?? 30),
  });
  const url = `${ALGOLIA_SEARCH_BY_DATE}?${params.toString()}`;
  const response = await fetch(url, {
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(`Algolia HN search_by_date failed: ${response.status} ${response.statusText}`);
  }
  const payload = (await response.json()) as { hits?: AlgoliaHit[] };
  return (payload.hits ?? [])
    .map((hit) => ({
      id: String(hit.objectID ?? ""),
      title: hit.title,
      url: hit.url,
    }))
    .filter((row) => row.id.length > 0);
}
