# MCP proof entry format

When the user asks for proof entries, output markdown blocks exactly in this shape (one per story):

```markdown
### [Story title](hn-url)
- **HN**: id {id}, score {score}, posted {iso-or-relative}
- **Tavily**: {source title} ({url}); {optional second source}
- **Synthesis**: 2–3 sentences on what MCP innovation angle this represents.
- **Confidence**: high | medium | low
```

## Correlation rubric

- **high**: Tavily finds independent coverage of the same product/announcement within 7 days.
- **medium**: Tavily finds related MCP ecosystem context but not the exact story.
- **low**: HN only, or Tavily unavailable, or weak keyword match.

Skip stories that are not meaningfully about MCP (Model Context Protocol), unless the run goal explicitly lists them.
