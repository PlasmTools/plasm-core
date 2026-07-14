/** MCP-aligned tool descriptions (intent-first; match plasm-core prompt_render assets). */

export const DISCOVER_TOOL_DESCRIPTION = `Plasm is a source language. Lexicon catalog browse for one user goal — this tool does **not** produce program symbols.
**Secondary path only.** On semantic auto-seed hosts, omit this tool and call \`plasm_context\` with \`session_mode: "new"\` + \`intent\` only.
When auto-seed is off: copy TSV \`api\`/\`entity\` rows into \`plasm_context\` \`seeds\`, then write \`plasm.program\` from the teaching TSV.`;

export const PLASM_CONTEXT_TOOL_DESCRIPTION = `**Default first MCP call.** Tool order: \`plasm_context\` → \`plasm\` (dry-run) → \`plasm_run\` (live). Do not start with discover on auto-seed hosts.

**Auto-seed hosts:** \`session_mode: "new"\` + \`intent\` only — omit \`seeds\` (rejected if passed). Host selects entities and returns teaching TSV on ready. On clarify/hard_miss, rephrase intent with the provider brand.

**When auto-seed is off / extend:** pass non-empty \`seeds\` (\`{api, entity}\`). Entity names resolve case-insensitively to catalog keys.

**One workflow → one \`logical_session_ref\`.** \`intent\` accumulates; it does not select the session. Use \`e#\`/\`m#\`/\`p#\`/\`r#\` from this session's teaching TSV only.

**Federated homonyms:** when multiple catalogs expose the same wire name, programs must use opaque symbols from this table — not bare wire names.

Returns \`logical_session_ref\` + fenced teaching TSV. \`_meta.plasm\`: \`logical_session_ref\`, \`continuity\`, \`domain_revision\`, optional \`relations\` / \`routing\`.`;

export const PLASM_TOOL_DESCRIPTION = `**Plan Plasm** (dry-run): **\`logical_session_ref\`** + **\`program\`**. Returns reviewable plan topology and executable **\`run_ref\`** (\`pcN\`). Pass that token to **\`plasm_run\`**; do **not** echo the program.

**\`program\` is Plasm source text, not JSON data.** Write one raw expression (e.g. \`e3(p15="electric").r2[p4,p5]\`) or multiline bindings with final roots. If a plan merely echoes an object/array/string literal, that is a literal no-op; rewrite as Plasm source.

Grammar below; symbols from \`plasm_context\` TSV. Reply with one valid plasm_program:

Output:
- Emit only code: one \`plasm_expr\`, or bindings then final roots. No prose, JSON, \`return\`, fences, or table rows.
- Prefer bind → narrow/project/transform → few final roots.

TSV table semantics:
- Header: \`plasm_expr<TAB>Meaning\`; one tab per row. Left = syntax/metadata; right = guidance only (never copy \`Meaning\`).
- Executable rows start with \`e#\`; metadata-only \`p#\`/\`r#\`/\`v#\` rows are never roots.

Symbol and fill rules:
- \`e#\` entity; \`m#\` method; \`p#\` field/param/filter; \`r#\` relation nav; \`v#\` value-domain metadata only.
- Relation hops: \`.r#\` (or the relation wire) on row producers — never a bare filter \`p#\` after \`.\` (\`p#\` in \`e#{…}\` is filter/param).
- Never write \`v#\` in code; use \`p#\` keys and read \`v#\` rows for allowed values.
- **Session handles** (\`logical_session_ref\`, \`run_ref\` / \`pcN\`): MCP tokens only — copy verbatim; not program syntax.
- Replace teaching placeholders (\`$\`, \`<id>\`, \`<val>\`) and exemplar ids (\`pikachu\`, \`example-name\`, …) with real values before output. Never emit \`$\` from a teaching row.
- Get-head: **parens = identity** (\`e_type(electric)\`; bare word = string identity when the slot is a name/string field), **braces = query/filter** (\`e#{p#=…}\`). Keyed \`e#(p#=<id>)\` only when that \`p#\` is the identity field.
- **Quote scalars only in \`{…}\` predicates** (\`e#{p#="EVA"}\`); identity ids in \`e#(…)\` need no quotes. EntityRef slots use \`e#(id)\`, not bare scalars.
- Remove \`..\` ellipses; add \`opt:\` keys (from \`Meaning\`) only as keyed assignments with real values.
- Projection \`[p#,…]\` suffixes: reuse only on expressions returning the same entity/list type.

Core surface:
- Program shape: one \`plasm_expr\`, or \`label = …\` bindings then comma-separated final roots (no \`return\`).
- Postfix/\`[p#]\`/\`[fields]\`: copy exact forms from the TSV left column (\`.limit\` \`.sort\` \`.filter{…}\` \`.group_by\` \`.aggregate\` …).
- Large list reads return the first **25 rows** in \`plasm_run\`; further pages copy the handle from the prior result's "more pages" line into **\`run_ref\`** on the next **\`plasm_run\`** (no second **\`plasm\`** call).
- \`page(...)\` is HTTP-execute program syntax only — not an MCP tool argument or \`plasm\` program.
- Search when exposed: \`e#~$\` or \`e#~"text"\` (bare \`e#~\` is a parse error); optional scoped filters \`e#~"text"[{p#=…}]\`.

Composition rules:
- One binding per line; final roots last (preferred). Single-line bindings coerced; default return is first binding.
- Row text: \`label = source <<TAG\` … \`TAG\`; optional \`label = source[p#,…] <<TAG\`. Program-level static bindings also accept \`label <<TAG\` (sugar for \`label = <<TAG\`). Minijinja \`{{ }}\` / \`{% %}\` over \`rows\` only inside the heredoc body — not \`\${}\`.
- Pass \`binding.content\` to string params; compose with \`\${report.content}\` in later heredocs/strings (\`$$\` escapes \`$\`). Do not use \`report.content\` as a final root or relation receiver.
- Action/create roots: return the action row or follow with \`e#(p#=…)\` get — not \`created.p#\` as a program root.
- Heredoc: \`<<TAG\` + newline; first trimmed \`TAG\` line closes; pick a tag absent from the body. \`markdown\`/\`html\`/\`document\`/\`json_text\`/\`blob\` values use \`<<TAG\` … \`TAG\` only.
- Worked shape (substitute symbols from your TSV):
items = e_source
filtered = items.filter{p_field>=300}
sorted = filtered.sort(p_field, desc).limit(10)
sorted[p_a,p_field]

Common pitfalls:
- Prefer \`label = e#\` then \`label.filter{…}\`; bare \`e#.filter{…}\` list-alls first. Row \`.filter{…}\` needs a materialized list.
- \`=>\` only for derive \`{ k: _.field }\` or write effects (\`source => e#.m#(…)\`) — not relation reads (use \`child = source.r#\`).
- Federated writes: one created-row return; discover anchors live — never hardcode workspace ids.
- **Symbols only:** use this session's \`e#\` / \`m#\` / \`r#\` / \`p#\` from the teaching TSV — not catalog type names, method verbs, or API labels. Unambiguous single-catalog wire names may parse, but always prefer taught \`e#\`/\`m#\`.
- Search operand: \`e#~$\` or \`e#~"text"\` required — bare \`e#~\` is a parse error.
- Federated sessions: when the same wire entity/method/relation/field name appears in multiple catalogs, bare wire tokens are compile errors — copy the session \`e#\` / \`m#\` / \`r#\` / \`p#\` from the teaching row for that catalog. Never write \`entry_id:Entity\` or \`catalog.Entity\` in programs.
- Search-only entities (no query): no \`e#{}\` list-all — scoped \`e#{p#=…}\` and/or \`e#~"text"\`.`;

export const PLASM_RUN_TOOL_DESCRIPTION = `**Run Plasm** (live): **\`logical_session_ref\`** + **\`run_ref\`** — a \`pcN\` plan commit from **\`plasm\`**, or the page handle from a prior result's "more pages" line. Real HTTP/API; may return **\`resource_link\`** snapshots.

**Review gate:** first live run executes the reviewed plan for the \`pcN\` **\`run_ref\`** from **\`plasm\`**. If the token is missing, expired, or from another plan, call **\`plasm\`** again.

**Paging:** when a result says \`more pages — call plasm_run with run_ref: "…"\`, copy that handle into **\`run_ref\`** on the next **\`plasm_run\`** (same **\`logical_session_ref\`**; no second **\`plasm\`** call). \`page(...)\` is HTTP-execute program syntax only — not an MCP tool argument.

**Live execute:** server spawns one async operation and awaits terminal rows in the tool response. Progress uses standard \`notifications/plasm/op\` on the registered handle.

**Live results:** \`## {return_label} ({n} rows)\` + capped TSV (25 rows inline); multi-return programs use \`# Results\` with \`### {label} ({n} rows)\` per root. ≤25 rows → inline \` \`\`tsv \`\`; 26–499 → capped TSV + snapshot URI (\`resources/read\` when the host supports it); 500+ → preview + snapshot, no inline TSV — narrow the program or page.`;
