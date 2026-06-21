/** Read-only operator shell — fetches `/operator/*` BFF JSON. */
export function renderOperatorShell(basePath = "/operator"): string {
  const base = basePath.replace(/\/$/, "");
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Plasm Operator</title>
  <style>
    :root { color-scheme: dark; font-family: ui-sans-serif, system-ui, sans-serif; }
    body { margin: 0; background: #0b0d10; color: #e8eaed; }
    header { padding: 1rem 1.25rem; border-bottom: 1px solid #22262d; display: flex; gap: 1rem; align-items: center; }
    header h1 { font-size: 1rem; font-weight: 600; margin: 0; letter-spacing: 0.02em; }
    nav { display: flex; gap: 0.5rem; flex-wrap: wrap; }
    nav button { background: #151922; border: 1px solid #2a3140; color: #c9d1d9; padding: 0.35rem 0.7rem; border-radius: 6px; cursor: pointer; }
    nav button.active { background: #1f6feb33; border-color: #388bfd; color: #fff; }
    main { padding: 1rem 1.25rem 2rem; }
    .meta { color: #8b949e; font-size: 0.85rem; margin-bottom: 1rem; }
    table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
    th, td { text-align: left; padding: 0.5rem 0.65rem; border-bottom: 1px solid #22262d; vertical-align: top; }
    th { color: #8b949e; font-weight: 500; }
    code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.82rem; }
    .badge { display: inline-block; padding: 0.1rem 0.45rem; border-radius: 999px; font-size: 0.75rem; }
    .ok { background: #23863633; color: #3fb950; }
    .warn { background: #9e6a0333; color: #d29922; }
    pre { background: #11151c; border: 1px solid #22262d; padding: 0.75rem; border-radius: 8px; overflow: auto; font-size: 0.8rem; }
    .error { color: #f85149; }
  </style>
</head>
<body>
  <header>
    <h1>Plasm Operator</h1>
    <nav id="nav"></nav>
  </header>
  <main>
    <div class="meta" id="meta"></div>
    <div id="content"></div>
  </main>
  <script>
    const BASE = ${JSON.stringify(base)};
    const PANES = [
      { id: "ops", label: "Ops", path: "/ops" },
      { id: "catalogs", label: "Catalogs", path: "/catalogs" },
      { id: "sessions", label: "Sessions", path: "/sessions" },
      { id: "plans", label: "Plans", path: "/plans" },
      { id: "runs", label: "Runs", path: "/runs" },
      { id: "traces", label: "Traces", path: "/traces" },
      { id: "archives", label: "Archives", path: "/archives" },
    ];
    let active = "catalogs";
    const nav = document.getElementById("nav");
    const meta = document.getElementById("meta");
    const content = document.getElementById("content");
    function setActive(id) {
      active = id;
      for (const btn of nav.querySelectorAll("button")) {
        btn.classList.toggle("active", btn.dataset.id === id);
      }
      void loadPane(id);
    }
    for (const pane of PANES) {
      const btn = document.createElement("button");
      btn.textContent = pane.label;
      btn.dataset.id = pane.id;
      btn.onclick = () => setActive(pane.id);
      nav.appendChild(btn);
    }
    async function fetchJson(path) {
      const res = await fetch(BASE + path);
      if (!res.ok) throw new Error(res.status + " " + path);
      return res.json();
    }
    function esc(s) {
      return String(s).replace(/[&<>"]/g, (c) => ({ "&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;" }[c]));
    }
    function renderTable(rows, columns) {
      if (!rows.length) return "<p>No rows.</p>";
      const head = columns.map((c) => "<th>" + esc(c.label) + "</th>").join("");
      const body = rows.map((row) =>
        "<tr>" + columns.map((c) => "<td>" + esc(c.render(row)) + "</td>").join("") + "</tr>"
      ).join("");
      return "<table><thead><tr>" + head + "</tr></thead><tbody>" + body + "</tbody></table>";
    }
    async function loadPane(id) {
      meta.textContent = "Loading…";
      content.innerHTML = "";
      try {
        const pane = PANES.find((p) => p.id === id);
        const data = await fetchJson(pane.path);
        meta.textContent = "Updated " + new Date().toLocaleString();
        if (id === "ops") {
          content.innerHTML = "<pre>" + esc(JSON.stringify(data, null, 2)) + "</pre>";
          return;
        }
        if (id === "catalogs") {
          content.innerHTML = renderTable(data.catalogs || [], [
            { label: "entry", render: (r) => r.entryId },
            { label: "hash", render: (r) => (r.catalogCgsHash || "").slice(0, 12) + "…" },
            { label: "stub", render: (r) => r.stub?.fresh ? "fresh" : "stale" },
            { label: "caps", render: (r) => String(r.capabilityCount ?? "") },
          ]);
          return;
        }
        if (id === "sessions") {
          content.innerHTML = renderTable(data.sessions || [], [
            { label: "intent", render: (r) => r.intent },
            { label: "ref", render: (r) => r.logicalSessionRef },
            { label: "plans", render: (r) => String(r.planCommitCount ?? 0) },
          ]);
          return;
        }
        if (id === "plans") {
          content.innerHTML = renderTable(data.plans || [], [
            { label: "pc", render: (r) => r.ref },
            { label: "intent", render: (r) => r.intent },
            { label: "program", render: (r) => (r.program || "").slice(0, 48) },
          ]);
          return;
        }
        if (id === "runs" || id === "traces" || id === "archives") {
          content.innerHTML = "<pre>" + esc(JSON.stringify(data, null, 2)) + "</pre>";
          return;
        }
        content.innerHTML = "<pre>" + esc(JSON.stringify(data, null, 2)) + "</pre>";
      } catch (err) {
        meta.innerHTML = '<span class="error">' + esc(String(err)) + "</span>";
      }
    }
    setActive("catalogs");
  </script>
</body>
</html>`;
}
