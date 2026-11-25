<div align="center">

<a href="https://github.com/corvin-chess"><img src="https://capsule-render.vercel.app/api?type=waving&height=200&color=0:0b0d12,55:161b2b,100:2f3d63&text=Corvin&fontColor=e6e9f2&fontSize=64&fontAlignY=45&animation=twinkling" alt="Corvin" width="100%" /></a>

<img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=500&size=16&color=8FA3D9&center=true&vCenter=true&width=560&height=34&duration=2800&pause=1200&lines=server;Axum+/health+/ws+play+wire,+Postgres,+Redis+presence+game+log,+auth,+move+apply,+clocks,+lag,+offers,+draw+claims,+quick+pair+%2B+challenge+links,+Glicko-2,+finished+game+persist+%2B+PGN,+protocol+contract+fixtures." alt="server" />

<p>
  <a href="https://github.com/corvin-chess/server"><img src="https://skillicons.dev/icons?i=rust&theme=dark" width="44" height="44" alt="server" /></a>&nbsp;
  <a href="https://github.com/corvin-chess"><img src="https://skillicons.dev/icons?i=github&theme=dark" width="44" height="44" alt="GitHub" /></a>
</p>

</div>

<br/>

Rust Axum game server for Corvin. Ships a bindable binary with `GET /health`, WebSocket `GET /ws` play wire (`Hello` / `Welcome` / `Ping` / `Pong`, cookie or `AuthTicket`, `QuickPair`, `Move`, offers, `Resign` / `Abort`, `ClaimDraw`, `Flag`, `ResumeGame` → `GameSnapshot` + `Replay`, and `GameEnd` with persist plus Glicko-2), reversible Postgres migrations for `users`, `sessions`, `passkeys`, `email_otps`, `challenges`, `ratings`, `rating_updates`, `games`, and `game_events`, Redis presence keys, Redis append-only live game event logs that replay to FEN via [chess-core](https://github.com/corvin-chess/chess-core), legal `apply_move` (Redis Lua CAS on event length) with PROTOCOL `MoveApplied` / `MoveRejected` (`ILLEGAL_MOVE`, `NOT_YOUR_TURN`, `GAME_GONE`, `AUTH`), server-authoritative clocks with Fischer increment and flag verification (`ClockSync` / `GameEnd` reason `flag`), lag compensation quota from server-measured RTT (`lag_comp_ms` on apply; never trusts client send time), draw / takeback / resign / abort helpers (`OfferState`, `OFFER_INVALID`, `ABORT_DENIED`, `GameEnd` reasons `agreement` / `resign` / `abort`), threefold / fifty-move `claim_draw` plus automatic fivefold / seventy-five draws (`CLAIM_DENIED`, `GameEnd` reasons `threefold` / `fifty` / `fivefold` / `seventy_five`), Redis quick pair pools plus Postgres challenge links that seat two players and emit PROTOCOL `GameStart`, Glicko-2 `apply_rated_finish` that updates per-time-class rating rows exactly once per `game_id` (PROTOCOL `rating_diff` `{w,b}`; skips casual and abort), once-only `persist_finished_game` into Postgres with `export_pgn` / `GET /api/v1/games/{id}/pgn` whose movetext matches chess-core `encode_movetext`, PROTOCOL contract fixtures under `tests/fixtures/protocol/` exercised by `tests/protocol_contract.rs`, HTTP auth under `/api/v1/auth` (WebAuthn passkeys plus email OTP), HttpOnly session cookies, CSRF-bound logout, and age-appropriate messaging defaults (`dm_enabled` / `public_chat_enabled`, minors stay off). Lobby seeks UI, tournaments, multi-instance hub fan-out, and the wider HTTP API remain Planned.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=OVERVIEW" alt="Overview" /></p>

Rust Axum server for Corvin. Current surface: `GET /health` returns HTTP 200 with plain body `ok` plus a trailing newline for Compose probes; `GET /ws` upgrades to the JSON play protocol v1 handshake (`Hello` with `client` `web|mobile|bot` and `locale` yields `Welcome` with ULID `session_id`, unix-ms `server_time`, and empty `features`, then a server `Ping` for RTT sampling; client `Ping`/`Pong` echo `nonce`; unsupported `v` returns `Error` `PROTO_VERSION`); reversible SQL migrations under `migrations/` create `users`, `sessions`, `passkeys`, `email_otps`, `challenges`, `ratings`, `rating_updates`, `games`, and `game_events`, and add `dm_enabled` / `public_chat_enabled` on `users` (ULID account ids, opaque sessions with CSRF secret material, WebAuthn credential JSON, hashed email OTP challenges; challenge rows hold open link tokens with expiry; ratings hold Glicko-2 `rating` / `rd` / `volatility` per time class; `rating_updates` audits one finish per `game_id`; `games` stores finished-game snapshots with username labels and `game_events` keeps the append-only JSONB audit log); Redis helpers under `src/redis_store.rs` connect via `REDIS_URL` and maintain short-TTL presence keys `corvin:presence:{user_id}`; game helpers under `src/game/` store live append-only logs at `corvin:game:{id}:meta` (initial FEN, seat ids, clocks with Fischer `increment_ms` and `active_started_ms`, `initial_ms` for PGN TimeControl, per-seat `lag_quota_*_ms`, `time_class`, `rated`, optional `pending_offer`, prev-clock snapshot for takeback) and `corvin:game:{id}:events` (JSON Move / Offer / OfferResponse / Resign / Abort / Takeback / ClaimDraw / AutoDraw / ServerNote entries with monotonic `seq`), reconstruct position FEN by replaying net Move UCIs through [chess-core](https://github.com/corvin-chess/chess-core) (Takeback pops the last net ply), apply seated legal moves via `apply_move` / `apply_move_with_rtt` (SAN + FEN + PROTOCOL reject codes; Redis Lua CAS on event list length so concurrent apply cannot double-append; deducts effective elapsed after lag credit then adds increment; flags before increment; optional `lag_comp_ms`; clears pending offers; may set `AppliedMove.auto_draw` for fivefold or seventy-five), and verify flag claims via `claim_flag` / `check_flag` (timeout draw when the opponent has no mating potential); offer helpers under `src/game/offers.rs` implement `offer_draw` / `offer_takeback` / `respond_offer` / `resign` / `abort` with PROTOCOL `OfferState` and `GameEnd` (`agreement`, `resign`, `abort`), reject codes `OFFER_INVALID` / `ABORT_DENIED`, ADR-005 first-move abort windows (bullet 15s / blitz 20s / rapid+ 60s), and counter-offer-as-accept; claim helpers under `src/game/claims.rs` implement `claim_draw` for side-to-move threefold or fifty-move claims (`CLAIM_DENIED` otherwise) and automatic fivefold / seventy-five ends after apply; lag helpers under `src/game/lag.rs` publish interim quota `clamp((initial+40×inc)/60, 200, 1000)` ms from server RTT EWMA one-way estimates (remeasure CV-305; never trust client send time); matchmaking helpers under `src/matchmaking/` implement Redis `quick_pair` / `quick_pair_cancel` / `take_pair_result` (exact-config pools `corvin:pair:pool:{time_class}:{rated|casual}:{initial_ms}:{increment_ms}`) and Postgres `challenge_create_link` / `challenge_accept` / `challenge_decline` / `challenge_cancel` that create seated live games (meta `rated` and `initial_ms` from config) and PROTOCOL `GameStart` JSON for both seats; rating helpers under `src/rating/` implement Glicko-2 (ADR-007: initial 1500/350/0.06, τ=0.75, provisional RD≥110) via `apply_rated_finish` / `game_end_with_rating_diff` (once-only via `rating_updates.game_id`; skip casual and abort; PROTOCOL `rating_diff` `{w,b}` integers; color weighting stays CV-221); persist helpers under `src/game/persist.rs` and `src/game/pgn.rs` implement once-only `persist_finished_game` / `persist_finished_from_redis` into `games` + `game_events`, plus `export_pgn` / `render_pgn` whose movetext matches chess-core `encode_movetext` of net Moves; PROTOCOL message shapes for built helpers are locked by golden fixtures under `tests/fixtures/protocol/` (`tests/protocol_contract.rs`); `GET /api/v1/games/{id}` and `GET /api/v1/games/{id}/pgn` serve the durable summary and PGN (`application/x-chess-pgn`); auth routes under `/api/v1/auth/email/otp/*` and `/api/v1/auth/passkey/*` mint HttpOnly `corvin_session` cookies (`SameSite=Lax`, `Secure` on https) and return a synchronizer `csrf_token` plus messaging flags (prior sessions for that user are revoked); `GET /api/v1/auth/session` resolves the cookie to `{user_id,username,csrf_token,dm_enabled,public_chat_enabled}`; `POST /api/v1/auth/logout` requires matching `X-CSRF-Token` and clears the cookie; `GET|PATCH /api/v1/account/messaging` reads or updates those flags (CSRF on PATCH; estimated age under 18 or missing `birth_year` cannot enable DMs or public chat). `GET /ws` on the full app authenticates via session cookie or `AuthTicket`, registers the seat on an in-process `PlayHub`, dispatches `QuickPair` / `QuickPairCancel`, `Move` (with Ping RTT into `LagTracker`), offer / resign / abort / claim / flag messages, and `ResumeGame` (`GameSnapshot` + `Replay`), and on terminal ends calls `persist_finished_from_redis` then `apply_rated_finish` before broadcasting `GameEnd`. Planned surfaces: expanding rating windows (CV-210), τ calibration (CV-220), tournaments, DM delivery (CV-281 uses `assert_can_send_dm`), mid-game Postgres flush (CV-180), multi-instance hub fan-out, and the wider HTTP API. Intended dependencies: PostgreSQL for durable data, Redis for live games and presence, chess-core for legality, and optional analysis via [engine](https://github.com/corvin-chess/engine). Fair-play review hooks are planned to consume the public [fairplay](https://github.com/corvin-chess/fairplay) library; private thresholds stay out of this repository. Outbound SMTP is not wired yet; OTP delivery uses a log sink unless tests inject a capture mailer.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=GETTING+STARTED" alt="Getting Started" /></p>

```bash
git clone https://github.com/corvin-chess/server.git
cd server
export DATABASE_URL=postgres://corvin:corvin_dev_only_change_me@127.0.0.1:5432/corvin
cargo run
```

Defaults: listen on `0.0.0.0:3000` (container-friendly). For local-only bind, set `HOST=127.0.0.1`. Override port with `PORT`. Check liveness with `curl -sf http://127.0.0.1:3000/health`. Serving the full router requires `DATABASE_URL` (auth state loads at process start).

Postgres migrations (for example from [infra](https://github.com/corvin-chess/infra) Compose):

```bash
export DATABASE_URL=postgres://corvin:corvin_dev_only_change_me@127.0.0.1:5432/corvin
cargo run -- migrate up
cargo run -- migrate down
```

Auth relying-party and OTP pepper (`CORVIN_OTP_PEPPER` is required unless `CORVIN_ENV` is `development` or `test`):

```bash
export CORVIN_ENV=development
export CORVIN_RP_ID=localhost
export CORVIN_RP_ORIGIN=http://localhost:3000
export CORVIN_OTP_PEPPER=dev-only-otp-pepper-change-me
```

Redis presence (requires `REDIS_URL` from the same Compose stack):

```bash
export REDIS_URL=redis://127.0.0.1:6379/0
```

Library helpers `touch_presence` / `is_online` / `clear_presence` use keys `corvin:presence:{user_id}` with a short TTL. Live game helpers `create_game` / `append_move_event` / `commit_apply_cas` / `commit_append_cas` / `apply_move` / `apply_move_with_rtt` / `reconstruct_fen` / `claim_flag` / `check_flag` / `seed_lag_quotas` / `offer_draw` / `offer_takeback` / `respond_offer` / `resign` / `abort` / `claim_draw` use `corvin:game:{id}:meta` and `corvin:game:{id}:events` (replay reconstructs FEN; `apply_move` rejects illegal and wrong-turn moves, ticks clocks with injectable server `ts`, and commits meta+Move via Lua CAS; offer/claim/resign/abort commit meta+events via `commit_append_cas`; bare `append_game_event` uses event-only Lua CAS (no meta rewrite) so concurrent non-move writers cannot share a seq; `apply_move_with_rtt` credits lag from a fake or measured RTT; `claim_flag` verifies PROTOCOL flag claims; offer helpers return `OfferState` or `GameEnd` and map rejects to `OFFER_INVALID` / `ABORT_DENIED`; `claim_draw` ends with `threefold` or `fifty` or rejects with `CLAIM_DENIED`; successful apply may auto-end with `fivefold` or `seventy_five`). Matchmaking helpers `quick_pair` / `quick_pair_cancel` / `take_pair_result` / `challenge_create_link` / `challenge_accept` / `challenge_decline` / `challenge_cancel` pair two players into a seated live game and return PROTOCOL `GameStart` (waiting peer notified via `corvin:pair:notify:{player_id}`; open links need `link_token` on accept). Rating helpers `apply_rated_finish` / `ensure_rating` / `get_rating` / `game_end_with_rating_diff` update Postgres `ratings` once per finished rated `game_id` (audit in `rating_updates`; casual and abort skip). Persist helpers `persist_finished_game` / `persist_finished_from_redis` / `export_pgn` / `render_pgn` write `games` + `game_events` once and rebuild standard tag-pair PGN (movetext matches chess-core `encode_movetext`).

WebSocket handshake (no Postgres required for the socket itself):

```bash
# After cargo run, open ws://127.0.0.1:3000/ws and send:
# {"typ":"Hello","v":1,"client":"web","locale":"en"}
# Expect Welcome, then a server Ping; reply with Pong using the same nonce.
```

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=DEVELOPMENT" alt="Development" /></p>

Edit `src/lib.rs` (router and `AppState`), `src/ws/` (PROTOCOL Hello/Welcome/Ping), `src/auth/` (email OTP, passkeys, sessions, mailer), `src/safety/` (age defaults and DM gate), `src/account/` (messaging prefs), `src/api/` (finished-game HTTP), `src/db.rs` (pool and migrate up/down), `src/redis_store.rs` (Redis connect and presence keys), `src/game/` (append-only game event log, FEN replay, legal `apply_move` with Lua/CAS commit, clocks with Fischer increment and flag, lag quota, draw/takeback/resign/abort offers, threefold/fifty claims and fivefold/seventy-five auto draws, Postgres persist and PGN export), `src/matchmaking/` (quick pair Redis pools, challenge links, `GameStart`), `src/rating/` (Glicko-2 math, Postgres ratings, once-only `apply_rated_finish`), `src/main.rs` (bind, graceful shutdown, migrate CLI), and SQL under `migrations/`. Local Postgres and Redis from [infra](https://github.com/corvin-chess/infra) supply `DATABASE_URL` / `REDIS_URL`. Secrets and fair-play private configuration never live in this repository.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=TESTING" alt="Testing" /></p>

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked --test protocol_contract
node scripts/verify-readme.mjs
node scripts/verify-dockerfile.mjs
```

Compose builds this image from the repository root `Dockerfile` (migrate-on-start entrypoint, `GET /health` on port 3000).

`tests/protocol_contract.rs` loads golden JSON under `tests/fixtures/protocol/` (manifest + Hello/Welcome/Ping/Pong/Error, MoveApplied, MoveRejected, ClockSync, OfferState, GameEnd, GameStart, Challenge), asserts required keys and the PROTOCOL.md error-code set, and compares public JSON builders to fixture shapes (handshake wire behavior stays covered by `tests/ws_hello.rs`; always-on; no Redis/Postgres). `tests/health.rs` asserts `/health` returns 200 with body `ok\n` and unknown paths return 404. `tests/ws_hello.rs` binds a local listener and contracts `Hello`/`Welcome`/`Ping`/`Pong` plus `PROTO_VERSION` against PROTOCOL.md (no Postgres). `tests/ws_play.rs` asserts `GameSnapshot` shape always-on and, when `DATABASE_URL` and `REDIS_URL` are set, two authenticated sockets quick-pair, play `e2e4`, and resign to a PROTOCOL `GameEnd`. `tests/migrations.rs` asserts paired up/down SQL and, when `DATABASE_URL` is set, applies migrate up/down against Postgres including `passkeys` / `email_otps` and messaging flags (CI runs Postgres 16). `tests/presence.rs` asserts presence key shape and, when `REDIS_URL` is set, PING plus touch/clear/TTL expiry against Redis (CI runs Redis 7). `tests/game_event_log.rs` asserts FEN replay for a known opening line always-on and, when `REDIS_URL` is set, create/append/reconstruct against Redis. `tests/move_apply.rs` asserts PROTOCOL `MoveApplied` / `MoveRejected` JSON shapes always-on and, when `REDIS_URL` is set, legal apply plus `ILLEGAL_MOVE` / `NOT_YOUR_TURN` / `AUTH` / `GAME_GONE` rejects. `tests/atomic_apply.rs` asserts Lua CAS reply parsing always-on and, when `REDIS_URL` is set, concurrent identical or racing white UCIs leave exactly one Move event. `tests/atomic_append.rs` asserts offer/claim CAS reply reuse always-on and, when `REDIS_URL` is set, concurrent resign, offer-vs-resign, dual draw offers, and bare `append_game_event` keep unique monotonic seqs with a single terminal End. `tests/clocks.rs` asserts injectable clock math (deduct then Fischer increment; flag before increment), PROTOCOL `ClockSync` / `GameEnd` shapes, and when `REDIS_URL` is set, persisted tick on `apply_move`, flag-on-apply, and `claim_flag` verification. `tests/lag_quota.rs` asserts interim quota bounds, EWMA one-way math, PROTOCOL optional `lag_comp_ms`, and when `REDIS_URL` is set, fake-RTT credit/deplete plus lag preventing a false flag. `tests/offers.rs` asserts ADR-005 abort windows, PROTOCOL `OfferState` / `GameEnd` / `OFFER_INVALID` / `ABORT_DENIED` shapes, and when `REDIS_URL` is set, draw accept/decline/withdraw/counter-offer, resign, abort (0–1 ply window), takeback FEN+clock restore, and auth rejects. `tests/draw_claims.rs` asserts PROTOCOL `GameEnd` reasons `threefold` / `fifty` / `fivefold` / `seventy_five` and `CLAIM_DENIED`, and when `REDIS_URL` is set, fifty and threefold claims by side to move, deny paths, and fivefold / seventy-five auto ends on apply. `tests/matchmaking.rs` asserts PROTOCOL `GameStart` / `Challenge` shapes always-on and, when `REDIS_URL` / `DATABASE_URL` are set, two-client quick pair, cancel, challenge link accept, decline/cancel, and self-accept reject. `tests/glicko2.rs` asserts the Glickman paper worked example (τ=0.5) plus ADR-007 defaults and provisional RD. `tests/ratings.rs` asserts PROTOCOL `rating_diff` shape always-on and, when `DATABASE_URL` is set, rated finish changes both rating rows once, returns `AlreadyApplied` on repeat, and skips casual/abort. `tests/persist_game.rs` asserts PGN movetext matches chess-core `encode_movetext` always-on and, when `DATABASE_URL` is set, once-only `persist_finished_game`, `export_pgn` parity, and `GET /api/v1/games/{id}/pgn` (Redis path when `REDIS_URL` is set). `tests/auth_email_otp.rs` and `tests/auth_passkey.rs` exercise register/login e2e when `DATABASE_URL` is set (capture mailer; SoftPasskey). `tests/session_csrf.rs` asserts cookie attribute matrix always-on and, when `DATABASE_URL` is set, CSRF-bound logout plus session probe security tests. `tests/age_defaults.rs` asserts under-18 / missing birth year default DMs off, adults can toggle via `/api/v1/account/messaging`, and minors cannot enable messaging. Compose stack image wiring and multi-instance hub fan-out land with later tasks.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=ARCHITECTURE" alt="Architecture" /></p>

Binary crate `corvin-server`: `src/lib.rs` owns the Axum router and `AppState` (Postgres, optional Redis, `PlayHub`); `src/ws/` implements PROTOCOL connection lifecycle plus play dispatch on `/ws` (`Hello`/`Welcome`/`Ping`/`Pong`, cookie/`AuthTicket`, `QuickPair`, `Move`, offers, resign/abort/claim/flag, `ResumeGame` → `GameSnapshot`/`Replay`, hub fan-out, finish persist + rating); `src/auth/` implements PROTOCOL auth paths with `webauthn-rs` and hashed email OTPs; `src/safety/` owns age-based messaging defaults and `assert_can_send_dm`; `src/account/` serves messaging preference GET/PATCH; `src/api/` serves finished-game `GET /api/v1/games/{id}` and `GET /api/v1/games/{id}/pgn`; `src/db.rs` embeds reversible sqlx migrations and exposes `migrate_up` / `migrate_down`; `src/redis_store.rs` opens a Redis connection manager from `REDIS_URL` and owns presence key helpers; `src/game/` owns live game meta, append-only event lists in Redis, FEN reconstruction, legal `apply_move` with Lua/CAS commit (PROTOCOL `MoveApplied` / `MoveRejected`), `commit_append_cas` for offer/claim/resign/abort and bare appends (CV-173), clocks (`src/game/clock.rs`: Fischer tick, `claim_flag`, `ClockSync` / `GameEnd`), lag compensation (`src/game/lag.rs`: EWMA RTT, quota, credit), offers (`src/game/offers.rs`: draw/takeback/resign/abort state machine), draw claims (`src/game/claims.rs`: threefold/fifty claim + fivefold/seventy-five auto), and durable persist/PGN (`src/game/persist.rs`, `src/game/pgn.rs`) via `corvin-chess-core`; `src/matchmaking/` owns Redis quick-pair pools and Postgres challenge links that create seated games and PROTOCOL `GameStart`; `src/rating/` owns Glicko-2 math (`glicko2.rs`) and once-only Postgres finish updates (`apply_rated_finish`); `src/main.rs` binds `HOST`/`PORT`, requires `DATABASE_URL` and `REDIS_URL`, and serves with graceful shutdown, or runs `migrate up|down` when those args are passed. Planned: multi-instance hub fan-out, expanding rating windows, tournaments. Server is authoritative for game state and clocks; clients reconcile to protocol events. See organization ADRs and [website](https://github.com/corvin-chess/website) once docs are published.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=STATUS" alt="Status" /></p>

| Area | State |
|---|---|
| README | Built |
| Axum scaffold | Built |
| Postgres migrations | Built |
| Redis presence | Built |
| Auth | Built |
| Session cookies / CSRF | Built |
| Age-appropriate defaults | Built |
| WS Hello/Welcome/Ping | Built |
| Game event log | Built |
| Move apply | Built |
| Atomic apply | Built |
| Atomic append | Built |
| Clocks | Built |
| Lag compensation | Built |
| Offers | Built |
| Draw claims | Built |
| Matchmaking | Built |
| Ratings | Built |
| Finished game persist | Built |
| Protocol contract fixtures | Built |
| Play protocol / WS | Built |
| Docker image | Built |
| Implementation | Planned |

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><sub><i>A move is real only when the server has written it down.</i></sub></p>

## License

This repository is licensed under AGPL-3.0-or-later.

<img src="https://capsule-render.vercel.app/api?type=waving&section=footer&height=110&color=0:0b0d12,55:161b2b,100:2f3d63" alt="" width="100%" />
