# D&D 5e SRD — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the public [D&D 5e API](https://www.dnd5eapi.co/) (SRD content). The catalog includes alignments, ability scores, damage types, languages, proficiencies, rules, feats, weapon properties, equipment categories, class level tables, spellcasting and multiclassing blocks, plus the original classes, spells, monsters, gear, and scoped list routes.

```bash
cargo run -p plasm-repl -- \
  --schema apis/dnd5e \
  --backend https://www.dnd5eapi.co
```

No authentication is required for the public API.