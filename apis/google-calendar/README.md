# Google Calendar REST API v3 — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [Google Calendar API v3](https://developers.google.com/calendar/api/v3/reference).

```bash
export GCAL_TOKEN="ya29.your_oauth_access_token"
cargo run -p plasm-repl -- \
  --schema apis/google-calendar \
  --backend https://www.googleapis.com/calendar/v3 \
  --repl
```

`domain.yaml` sets **`http_backend: https://www.googleapis.com/calendar/v3`** so CML paths (`users/me/calendarList`, `calendars/{calendarId}/events`, …) are appended to that origin. Do not omit the `/calendar/v3` suffix.

---

## CGS design notes

### Compound key for events: `calendarId` + `id`

Google Calendar events are not globally addressable. The stable key is `GET /calendars/{calendarId}/events/{eventId}` — both path segments are required. The CGS models this as compound `key_vars`:

```yaml
Event:
  key_vars: [calendarId, id]
```

`calendarId` is modeled as **`entity_ref` → `Calendar`** so FK navigation and scope splat match the rest of Plasm’s Google APIs.

The runtime injects both `calendarId` and `id` into the CML environment from the compound `Ref`, so `event_get` paths resolve without extra flags.

```bash
# CLI: get a specific event (compound id)
event primary/abc123def456

# REPL
Event("primary/abc123def456")
```

`primary` is Google’s alias for the user’s primary calendar. Other calendar IDs look like `user@gmail.com` or `abc123@group.calendar.google.com`.

### Nested `EventDateTime` and people

Events use nested `start` / `end` (`date` vs `dateTime`), nested `organizer` / `creator`, etc. **`path:`** on fields in `domain.yaml` maps these into flat Plasm fields (e.g. `start.dateTime` → `start_datetime`). All-day events populate `start_date` / `end_date` instead of the `*_datetime` fields.

Rich subtrees that agents often need verbatim (**`attendees`**, **`recurrence`**, **`reminders`**, **`conferenceData`**, **`extendedProperties`**) are modeled as **`json`** or **`array`/`items`** so the wire shape is preserved without exploding the CGS into dozens of micro-entities.

### Calendar list as the `Calendar` entity

Google exposes both **`calendarList`** (per-user subscription metadata: colors, access role, `primary`, …) and **`calendars`** (calendar resource). This CGS uses **calendarList** `GET` / `list` as the read surface because it is what agents need to discover “which calendars can I see?” and includes **`accessRole`**, **`backgroundColor`**, etc.

### Relation: `Calendar` → `Event`

The `events` relation uses **`materialize: query_scoped_bindings`** so chain traversal (`Calendar("primary").events`) resolves to **`event_list`** with `calendarId` bound from the parent calendar’s `id`:

```yaml
relations:
  events:
    target: Event
    cardinality: many
    materialize:
      kind: query_scoped_bindings
      capability: event_list
      bindings:
        calendarId: id
```

### `timeMin` / `timeMax` (official API semantics)

Per the [events.list](https://developers.google.com/calendar/api/v3/reference/events/list) reference, these bounds filter which events are returned using the API’s **overlap** rules (each bound is **exclusive**):

- **`timeMax`** — upper bound (exclusive) for an event’s **start** time.
- **`timeMin`** — lower bound (exclusive) for an event’s **end** time.

Always pass **RFC 3339** timestamps **with a time-zone offset** (e.g. `2011-06-03T10:00:00-07:00` or `Z`). Do not assume “min = range start, max = range end” in the naive wall-clock sense without reading the API doc above.

### OAuth 2.0

The schema uses **`bearer_token`** with **`GCAL_TOKEN`**. See [Authentication](#authentication) below. The **`oauth:`** block in `domain.yaml` lists Google’s scope strings, default bundles, and which scopes satisfy each capability for control-plane filtering.

---

## What is implemented (read surface)

| Entity | Key | Notes |
|--------|-----|--------|
| **`Calendar`** | `id` | From **calendarList**; fields include list metadata (`accessRole`, `primary`, colors, …). **`primary_read: calendar_get`**. |
| **`Event`** | `calendarId` + `id` (compound) | From **events**; nested times, organizer/creator, JSON blobs for attendees/reminders/conference, etc. **`primary_read: event_get`**. |

| Capability | Kind | HTTP (v3) |
|------------|------|-----------|
| `calendar_get` | get | `GET /users/me/calendarList/{calendarId}` |
| `calendar_list` | query | `GET /users/me/calendarList` (cursor pagination) |
| `event_get` | get | `GET /calendars/{calendarId}/events/{eventId}` |
| `event_list` | query (scoped) | `GET /calendars/{calendarId}/events` (cursor pagination; rich query params) |

**`event_list`** supports filters including `q`, `timeMin`, `timeMax`, `timeZone`, `updatedMin`, `syncToken`, `orderBy`, `singleEvents`, `showDeleted`, `eventTypes`, `iCalUID`, `maxAttendees`, `showHiddenInvitations`, `privateExtendedProperty`, `sharedExtendedProperty`, etc. See `domain.yaml` for the full parameter set wired in **`mappings.yaml`**.

---

## REPL / expression examples

```
Calendar
Calendar("primary")
Event{calendarId=primary}
Event{calendarId=primary, timeMin=2024-01-01T00:00:00Z, timeMax=2024-01-31T23:59:59Z}
Calendar("primary").events
Event("primary/abc123def456")
Event{calendarId=primary}[summary, start_datetime, end_datetime, location]
```

---

## CLI examples

```bash
export GCAL_TOKEN="ya29.your_oauth_access_token"
BASE="--schema apis/google-calendar --backend https://www.googleapis.com/calendar/v3"

plasm $BASE calendar query
plasm $BASE calendar primary
plasm $BASE event list \
  --calendarId primary \
  --timeMin 2024-01-01T00:00:00Z \
  --timeMax 2024-01-31T23:59:59Z \
  --singleEvents \
  --orderBy startTime \
  --limit 50
plasm $BASE event list --calendarId primary --q "standup" --limit 20
plasm $BASE event list --calendarId primary --all
plasm $BASE event primary/abc123def456
plasm $BASE --output table event list \
  --calendarId primary \
  --timeMin 2024-01-15T00:00:00Z \
  --timeMax 2024-01-16T00:00:00Z \
  --singleEvents
```

---

## Authentication

Google Calendar requires OAuth 2.0. Set **`GCAL_TOKEN`** to a valid access token.

### Option 1: gcloud CLI (quick tests)

```bash
gcloud auth login
export GCAL_TOKEN=$(gcloud auth print-access-token)
```

### Option 2: OAuth 2.0 Playground

1. https://developers.google.com/oauthplayground/
2. Select **Google Calendar API v3** → e.g. `https://www.googleapis.com/auth/calendar.readonly`
3. Authorize → exchange → copy **Access token**

```bash
export GCAL_TOKEN="ya29...."
```

### Option 3: Service account

The calendar must be shared with the service account email, or use domain-wide delegation.

---

## Scope quick reference

| Use case | Scope |
|----------|--------|
| Read calendars + events | `https://www.googleapis.com/auth/calendar.readonly` |
| Read/write events | `https://www.googleapis.com/auth/calendar.events` or `.../calendar` |
| Full access | `https://www.googleapis.com/auth/calendar` |

The CGS **`oauth.default_scope_sets`** and **`oauth.requirements`** mirror these for tooling.

---

## Modeling audit notes (maintenance)

- **Writes / free-busy / ACL / settings** are **not** in this CGS yet (`event_create`, `freeBusy`, `acl`, `users.me/settings`, etc.). See Google’s REST reference for the full surface.
- **List rows vs GET**: Event resources returned by **`event_list`** may omit fields unless you hydrate with **`event_get`**; compound refs for list rows depend on runtime identity injection for `calendarId` (query scope). Prefer explicit **`event_get`** when you need full **`attendees`** / conference data.
- **Incremental sync**: **`syncToken`** / **`nextSyncToken`** pair is supported on **list**; follow Google’s [sync guide](https://developers.google.com/calendar/api/guides/sync) — certain parameters are mutually exclusive with `syncToken`.
- **Eval coverage**: Run `plasm-eval coverage --schema apis/google-calendar --cases apis/google-calendar/eval/cases.yaml`.

---

## Testing status

Schema validates (`plasm schema validate apis/google-calendar/`). CLI help and pagination flags should be exercised after changes. Live API testing requires **`GCAL_TOKEN`**.

---

## Known limitations

- **Token expiry**: OAuth access tokens expire (often ~1h). Long-running agents need refresh or service-account token rotation.
- **Backend**: Must be **`https://www.googleapis.com/calendar/v3`** (matches `http_backend` in `domain.yaml`).
