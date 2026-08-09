# Open-Meteo — Plasm CGS Schema

A [Plasm](../../README.md) domain model for [Open-Meteo](https://open-meteo.com/) (weather and geocoding APIs).

```bash
cargo run -p plasm-repl -- \
  --schema apis/openmeteo \
  --backend https://api.open-meteo.com
```

Example REPL program (wire names from teaching TSV; substitute session `e#` / `m#`):

```text
e1.m1(latitude=40.7, longitude=-74, current_weather=true)
```

No API key for non-commercial use; see Open-Meteo terms for production. See [apis/README.md](../README.md) for catalog layout and [docs/saas-architecture.md](../../docs/saas-architecture.md) for hosted deployment context.
