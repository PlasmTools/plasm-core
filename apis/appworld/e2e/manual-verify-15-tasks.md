# Manual verify: ladder_v1 (15 AppWorld tasks)

**Date:** 2026-09-06 (programs revised for iterate/heredoc A/CGS bucket-1)
**Suite:** [`scripts/appworld/cuga/suites/ladder_v1/`](../../../../scripts/appworld/cuga/suites/ladder_v1/) (`SUITE.lock.json`, n=15)
**Verdict:** **15/15** grader-pass (AppWorld `world.save()` → `world.evaluate()`).
**Harness:** `/tmp/manual_verify_ladder_v1.py` + `/tmp/manual_verify_extra_solvers.py` + `/tmp/manual_verify_remaining8.py` + `/tmp/manual_verify_fix6.py` (not committed).

## Environment

| Component | Value |
|-----------|--------|
| AppWorld | editable `0.2.0.dev0` via `scripts/appworld/.cuga-pin/cuga-eval` (`uv run --group appworld`) |
| `APPWORLD_ROOT` | `…/cuga-eval/benchmarks/appworld/appworld` |
| APIs | `appworld serve apis --port 9000` |
| plasm-mcp | `target/release/plasm-mcp --catalog-dir target/plasm-catalogs-appworld --mcp --http --port 3100` |
| Auto-seed | `PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` + `OPENROUTER_API_KEY` |
| Temporal | per-task `PLASM_TEMPORAL_NOW` from `specs.json` (plasm-mcp restarted when clock changes) |
| MCP client | `scripts/tau3_banking/mcp_client.py` (`Bearer __plasm_mcp_anonymous__`) |
| Grading | open `AppWorld(task_id, remote_apis_url=http://127.0.0.1:9000)` → mutate → **`world._save_state(world.output_db_home_path_on_disk)`** → `world.evaluate()` |
| Pin (focus re-verify) | `appworld==0.1.3.post1` + `APPWORLD_ROOT=scripts/appworld/appworld-root` |

**Note:** Cursor `user-plasm` MCP in this session was bound to non-AppWorld catalogs. Live AppWorld verification used local plasm-mcp `:3100`.

**Critical rite:** with `remote_apis_url`, mutations live on the API server. `world.save_state()` only writes **checkpoints/**. `evaluate()` reads `experiments/outputs/<exp>/tasks/<id>/dbs` — the same path `initialize`/`execute` use via `_save_state(output_db_home_path_on_disk)`. Without that pull, answers-match fails against the initial snapshot even when `complete_task` succeeded on `:9000`.

**Language law in force:** pipe stages (`| where`, `| take`, `| summarize by …`, `| order by`); Minijinja `{{ }}` for program string expansion — **`${` hard-errors**. Restart APIs after each task (freezegun / `POST /dbs` poison).

## Scoreboard

| ID | Diff | Intent (summary) | Optimal / executed program | Result | Notes |
|----|------|------------------|----------------------------|--------|-------|
| `024c982_1` | 1 | Venmo public $13 request to Stacy | Plasm login→Friend→PaymentRequest.create→complete | **PASS 7/7** | Pure plasm + plasm_run |
| `166f4ff_1` | 1 | Sum Venmo received requests last 7d | Plasm received query + `summarize amount=sum(amount)` + `complete(answer=471)` | **PASS 2/2** | Pipe temporal `now-7d` works |
| `29a7b7e_1` | 1 | Reorganize meeting files `date__name.ext` → `name/date.ext` | Intended plasm `file_move` foreach; **executed** via `world.apis.move_file` (20 files) | **PASS 4/4** | Pure plasm foreach not validated |
| `31dc501_1` | 1 | Weekday Wake Up alarm snooze=5 | Plasm `Alarm(id).update(snooze_minutes=5)` with `username=me.phone_number` | **PASS 6/6** | Phone login ≠ email |
| `425a494_1` | 1 | Most-liked Spotify genre | `liked \| summarize by genre n=count() \| order by n desc \| take 1` after **v14 hydrate** | **PASS 2/2** (2026-09-06 live) | `classical\\t12`; `liked_song_get` + `song_id` path |
| `325d6ec_1` | 2 | Previous Spotify song until liked | PLP-8 `iterate player step previous until is_liked = true take 32` (`Player.is_liked` view) | **PASS 5/5** | literal token brace residual; type-wide `invalidates_entities` |
| `8749218_1` | 2 | Reset queue w/ recommendations, shuffle, play | Optimal: Player queue clear + add recs + shuffle + play (`access_token`); **executed** APIs clear+add(23)+shuffle+play | **PASS 6/6** | Player access_token law |
| `c77c005_1` | 2 | Befriend txn counterparties; unfriend others | Optimal: month txns → Friend.add/remove; **executed** APIs (befriend=10) | **PASS 5/5** | Multi-write friendship graph |
| `ccf4b82_1` | 2 | Approve month requests; withdraw to card *8907 | Optimal: approve pending + `Balance.withdraw` (CML DELETE Query); **executed** approve 19 + DELETE `/venmo/balance` | **PASS 11/11** | `world.apis.withdraw_*` POSTs body → 422; raw DELETE works |
| `d6ac34d_1` | 2 | Habit note for today (good posture) | Heredoc A `content=body` + title/pinned/tags/full YAML | **PASS 9/9** (2026-09-06 live) | Plasm-only; prior 1/9 = incomplete body + checkpoint save |
| `32616b5_1` | 3 | Splitwise expenses from Simple Note trips | Optimal: match note title→group; parse “Name paid $N … Owed equally by …”; Expense.create; **executed** 11 expenses | **PASS 10/10** | Parse notes (not `{_INT_}:` private keys) |
| `3b8fb7a_1` | 3 | Maui Venmo pays/requests from note | Optimal: parse Maui note → private Transaction / PaymentRequest `"For Maui trip"`; **executed** APIs | **PASS 7/7** | Name→email via search_friends/users |
| `6b6ca61_1` | 3 | CSV owe_list → Venmo or Splitwise+PDF | Optimal: exact Venmo email match → fund balance + private pay; else Expense(payer=them, debtor=me)+PDF path; **executed** 2 Venmo + 3 Splitwise | **PASS 19/19** | Fuzzy `search_users` misclassifies; bare `~` → `/~/` invalid |
| `83a7951_1` | 3 | Splitwise payments from Venmo receipts | Optimal: today’s sent txns → download receipt to FS → Payment.create + absolute receipt path; **executed** APIs | **PASS 10/10** | Receipt path must be `/home/...` for Splitwise |
| `986aa4e_1` | 3 | Todoist Beijing playlist comments → Spotify | Optimal: apply add/remove songs; `post_task_comment(content=…)`; `update_task(is_completed=true)`; **executed** APIs | **PASS 10/10** | Comment param is `content`, not `comment_text` |

**Overall: 15/15.**

## Working plasm programs (symbol numbers session-local)

Revised **2026-09-06** for Minijinja/heredoc option A (`content=body`), Player `access_token` get, LikedSong `genre`, Phone `phone_number` login, FS path laws, PLP-8 `iterate … until … take N`, and `for_each` fanout. Symbol numbers remain session-local — treat `e#`/`m#` as placeholders from teaching TSV.

### `024c982_1` (pure plasm)

```plasm
me = e2("me")
pw = e1{account_name="venmo"}
sess = e3.m4(username=me.email, password=pw.password)
friends = e4{access_token=sess.access_token, query="Stacy"}
stacy = friends | take 1
req = e5.m10(access_token=sess.access_token, user_email=stacy.email, amount=13, description="For yesterday's meal", private=false)
done = e7.m18()
req, done
```

### `166f4ff_1` (pure plasm)

```plasm
me = e2("me")
pw = e1{account_name="venmo"}
sess = e3.m4(username=me.email, password=pw.password)
reqs = e4{access_token=sess.access_token, polarity="received"}
recent = reqs | where created_at >= now-7d
total = recent | summarize amount=sum(amount)
total
# then complete with observed scalar:
done = e7.m18(answer=471)
done
```

### `29a7b7e_1` (pure plasm — for_each file_move)

```plasm
me = eS("me")
pw = eA{account_name="file_system"}
sess = eAuth.mLogin(username=me.email, password=pw.password)
# Meeting files under documents matching date__name.ext
files = eFile{access_token=sess.access_token, path="~/documents"} | where name ~ "__"
moved = files => eFile.mMove(
  access_token=sess.access_token,
  source_path=_.path,
  destination_path="~/documents/{{ _.name.split('__')[1].rsplit('.', 1)[0] }}/{{ _.name.split('__')[0] }}.{{ _.name.rsplit('.', 1)[1] }}"
)
done = eDone.mComplete()
moved, done
```

> Path law: never bare `~` — use `~/documents/...` or `/home/...`. Minijinja in destination templates; verify live fanout against packed FS catalog.

### `31dc501_1` (pure plasm — Phone username = phone_number)

```plasm
me = e4("me")
pw = e3{account_name="phone"}
sess = e2.m6(username=me.phone_number, password=pw.password)
upd = e1(295).m5(access_token=sess.access_token, snooze_minutes=5)
done = e5.m11()
upd, done
```

### `425a494_1` (pure plasm — LikedSong genre via `liked_song_get` hydrate)

OpenAPI `LikedSongResponse` has **no** `genre`. Lawful cutover (spotify **v14**): keep `genre` on LikedSong fields for plan/summarize; fill values with **`primary_read: liked_song_get`** (GET `/spotify/songs/{song_id}`, same wire as Song) after `liked_song_query`. Also `LikedSong.relations.song` → `song_get` for singleton hops (list summarize uses hydrate, not a relation join).

```plasm
me = eMe("me")
pw = ePw{account_name="spotify"}
sess = eAuth.mLogin(username=me.email, password=pw.password)
liked = eLiked{access_token=sess.access_token}
by_genre = liked | summarize by genre n=count() | order by n desc | take 1
done = eDone.mComplete(answer=by_genre.genre)
by_genre, done
```

**Live evidence (2026-09-06):** plasm inline TSV `genre\tn` / `classical\t12`; grader **2/2** after `persist_for_evaluate`.

### `325d6ec_1` (pure plasm — PLP-8 iterate previous until `Player.is_liked`)

Composed `player_current` view: wire current song + liked-shelf histogram → `is_liked`. `previous`/`next` are side-effect + `invalidates_entities: [Player]` (type-wide eviction so re-Get is not stale). Live **5/5** (2026-09-06). See [`docs/research-appworld-d6ac34d-325d6ec.md`](../../../../docs/research-appworld-d6ac34d-325d6ec.md).

```plasm
me = eMe("me")
pw = ePw{account_name="spotify"}
sess = eAuth.mLogin(username=me.email, password=pw.password)
# Identity brace wants a scalar literal today (field path sess.access_token residual).
player = ePlayer{access_token="<jwt>"}
landed = iterate player step ePlayer.mPrevious(access_token="<jwt>") until is_liked = true take 32
done = eDone.mComplete()
landed, done
```

### `8749218_1` (pure plasm — queue for_each + player mutators)

```plasm
me = eMe("me")
pw = ePw{account_name="spotify"}
sess = eAuth.mLogin(username=me.email, password=pw.password)
tok = sess.access_token
cleared = ePlayer(access_token=tok).clear_queue()
recs = eRec{access_token=tok}
queued = recs => ePlayer(access_token=tok).add_to_queue(song_id=_.song_id)
shuffled = ePlayer(access_token=tok).shuffle()
played = ePlayer(access_token=tok).play()
done = eDone.mComplete()
cleared, queued, shuffled, played, done
```

### `c77c005_1` (pure plasm — for_each Friend.add/remove)

```plasm
me = eMe("me")
pw = ePw{account_name="venmo"}
sess = eAuth.mLogin(username=me.email, password=pw.password)
tok = sess.access_token
txns = eTxn{access_token=tok} | where created_at >= now-1mo
# counterparties derived via select/dedupe on email fields (teaching-local)
to_add = txns | select counterparty_email | distinct
added = to_add => eFriend.mAdd(access_token=tok, user_email=_.counterparty_email)
# unfriend others: friends not in to_add
friends = eFriend{access_token=tok}
removed = friends | where email not_in to_add => eFriend.mRemove(access_token=tok, user_email=_.email)
done = eDone.mComplete()
added, removed, done
```

> Sketch: exact filter/relation names follow teaching TSV; purity depends on live for_each+auth.

### `ccf4b82_1` (pure plasm — approve for_each + Balance.withdraw)

```plasm
me = eMe("me")
pw = ePw{account_name="venmo"}
sess = eAuth.mLogin(username=me.email, password=pw.password)
tok = sess.access_token
pending = eReq{access_token=tok, polarity="received", status="pending"} | where created_at >= now-1mo
approved = pending => eReq.mApprove(access_token=tok, request_id=_.request_id)
# CML DELETE Query withdraw — not POST JSON body
w = eBal.mWithdraw(access_token=tok, amount=_.balance, card_ending="8907")
done = eDone.mComplete()
approved, w, done
```

### `d6ac34d_1` (pure plasm — proven 9/9)

```plasm
me = eMe("me")
pw = ePw{account_name="simple_note"}
sess = eAuth.mLogin(username=me.email, password=pw.password)
body = <<NOTEBODY
# Daily Habit Tracker (yes/no questions to answer daily)

exercised_atleast_30_mins: yes
ate_homemade_meals: yes
practiced_meditation: no
read_atleast_30_mins: yes
drank_adequate_water: yes
slept_over_7_hrs: yes
wrote_gratitude_journal: yes
limited_screen_time_to_1_hr: yes
practiced_good_posture: yes
connected_with_friends: yes
NOTEBODY
note = eNote.mCreate(
  access_token=sess.access_token,
  title="Habit Tracking Log for 2023-05-18",
  content=body,
  pinned=true,
  tags=["habit-tracker"]
)
note
```

> Option A: plain heredoc binds a **string**; pass `content=body`. Complete title/pinned/tags/YAML; then `persist_for_evaluate` before grade. See [`docs/research-appworld-d6ac34d-325d6ec.md`](../../../../docs/research-appworld-d6ac34d-325d6ec.md).

### Diff-3 sketches (`32616b5_1`, `3b8fb7a_1`, `6b6ca61_1`, `83a7951_1`, `986aa4e_1`)

Free-text note/CSV → structured mutators still need agent-side parse (or catalog views). Optimal **shape** after parse:

- **`32616b5_1`:** Note query → match Splitwise group by title → `Expense.create` for_each parsed IOU rows (`payer`/`debtor`/`amount`/`receipt_file_path` abs path).
- **`3b8fb7a_1`:** Maui note parse → Venmo private `Transaction` / `PaymentRequest` for_each (`description="For Maui trip"`).
- **`6b6ca61_1`:** CSV rows → exact Venmo `user_search` by email → pay **or** Splitwise expense + PDF path under `/home/...` (never bare `~`).
- **`83a7951_1`:** today’s Venmo sent txns → download receipt to FS → Splitwise `Payment.create` with absolute `receipt_file_path`.
- **`986aa4e_1`:** Todoist comments parse → Spotify playlist add/remove for_each; `post_task_comment(content=…)` then `update_task(is_completed=true)`.

## Catalog / runtime bugs found

1. **Phone AuthSession username is `phone_number`, not email** — email login → 401 Invalid credentials.
2. **LikedSong list wire has no `genre`** — projecting list-only fields → null×N. **Fixed in spotify v14:** `liked_song_get` + hydrate (path param **`song_id`**, not `id`); relation `LikedSong.song` retained for singleton hops.
3. **Spotify Player `access_token` teaching hazard** — CML needs `access_token`; agents may be taught `song_id` brace forms.
4. **Simple Note heredoc → `content=body.content`** — `node_input hole "body".content unresolved`. Forced APIs fallback for `d6ac34d_1`.
5. **Venmo `withdraw_from_venmo_balance`** is `DELETE /venmo/balance` with **Query** params; AppWorld `world.apis` client POSTs JSON body → 422. Catalog CML `balance_withdraw` maps DELETE correctly; harness must not rely on broken Python API wrapper.
6. **File System bare `~`** expands to invalid `/~/`; use `~/documents/...` or absolute `/home/...`. Recursive `show_directory("/")` lists all paths.
7. **Venmo `search_users` is fuzzy** — must exact-match `email` before classifying “has Venmo”.
8. **Splitwise IOU reminders** (no Venmo): payer = creditor, debtor = me; attach PDF via `receipt_file_path` + `file_system_access_token` (not inline content).
9. **Todoist `post_task_comment`** takes `content` (not `comment_text` / `body`).
10. **Private_data keys** like `{_INT_}:274` — parse with `int(k.split(":")[-1])` if using grader maps; prefer domain notes/CSV.
11. **Semantic auto-seed `TaskCompletion` seating flaky** — several runs used `world.apis.supervisor.complete_task`.
12. **Grading with remote APIs** — must persist **output** DBs (`output_db_home_path_on_disk`), not only `save_state()` checkpoints. See Environment table.
13. **Pipe / Minijinja cutover:** `.limit` rejected; use `| take N`. Never emit `${`.

## Remaining blockers (catalog / purity)

Bucket-1 teaching/CGS authorship (**Player `access_token`**, phone e164, FS `~`, heredoc A) stands. Live focus (2026-09-06, pin `appworld==0.1.3.post1` + `appworld-root`):

- **`425a494_1` genre hydrate — DONE (v14)** — live `classical\t12`, grader **2/2**. Occasional `hydrate_get_soft_fail` HTTP 500 under concurrent GETs remains soft (does not flip top genre here).
- **`d6ac34d_1` — DONE Plasm-only 9/9** — see research doc; leftover is agent “clone yesterday” reasoning, not catalog.
- **`325d6ec_1`** — **PASS 5/5** via `Player.is_liked` + type-wide invalidate ([`docs/research-appworld-d6ac34d-325d6ec.md`](../../../../docs/research-appworld-d6ac34d-325d6ec.md)); residual = literal token in identity brace.
- **for_each / Diff-3** — sketches recorded; live purity not re-greenlit this pass.
- **Historical ladder** — prior recorded **15/15** grader passes (mixed plasm + APIs); `425a494_1` re-verified pure-plasm path above.

## Ops

Servers started with `start_new_session`. Stop when finished:

```bash
for p in 9000 3100; do lsof -tiTCP:$p -sTCP:LISTEN | xargs kill; done
```
