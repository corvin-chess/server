<div align="center">

<a href="https://github.com/corvin-chess"><img src="https://capsule-render.vercel.app/api?type=waving&height=200&color=0:0b0d12,55:161b2b,100:2f3d63&text=Corvin&fontColor=e6e9f2&fontSize=64&fontAlignY=45&animation=twinkling" alt="Corvin" width="100%" /></a>

<img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=500&size=16&color=8FA3D9&center=true&vCenter=true&width=560&height=34&duration=2800&pause=1200&lines=server;Axum+/health+/ws+handshake,+Postgres,+Redis+presence+game+log,+auth,+move+apply.+Lobby+Planned." alt="server" />

<p>
  <a href="https://github.com/corvin-chess/server"><img src="https://skillicons.dev/icons?i=rust&theme=dark" width="44" height="44" alt="server" /></a>&nbsp;
  <a href="https://github.com/corvin-chess"><img src="https://skillicons.dev/icons?i=github&theme=dark" width="44" height="44" alt="GitHub" /></a>
</p>

</div>

<br/>

Rust Axum game server for Corvin. Ships a bindable binary with `GET /health`, WebSocket `GET /ws` handshake (`Hello` / `Welcome` / `Ping` / `Pong` per PROTOCOL.md), reversible Postgres migrations for `users`, `sessions`, `passkeys`, and `email_otps`, Redis presence keys, Redis append-only live game event logs that replay to FEN via [chess-core](https://github.com/corvin-chess/chess-core), legal `apply_move` with PROTOCOL `MoveApplied` / `MoveRejected` (`ILLEGAL_MOVE`, `NOT_YOUR_TURN`, `GAME_GONE`, `AUTH`), HTTP auth under `/api/v1/auth` (WebAuthn passkeys plus email OTP), HttpOnly session cookies, CSRF-bound logout, and age-appropriate messaging defaults (`dm_enabled` / `public_chat_enabled`, minors stay off). Lobby, rated play, tournaments, WS move dispatch, clocks, and the wider HTTP API remain Planned.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=OVERVIEW" alt="Overview" /></p>

Rust Axum server for Corvin. Current surface: `GET /health` returns HTTP 200 with plain body `ok` plus a trailing newline for Compose probes; `GET /ws` upgrades to the JSON play protocol v1 handshake (`Hello` with `client` `web|mobile|bot` and `locale` yields `Welcome` with ULID `session_id`, unix-ms `server_time`, and empty `features`, then a server `Ping` for RTT sampling; client `Ping`/`Pong` echo `nonce`; unsupported `v` returns `Error` `PROTO_VERSION`); reversible SQL migrations under `migrations/` create `users`, `sessions`, `passkeys`, and `email_otps`, and add `dm_enabled` / `public_chat_enabled` on `users` (ULID account ids, opaque sessions with CSRF secret material, WebAuthn credential JSON, hashed email OTP challenges); Redis helpers under `src/redis_store.rs` connect via `REDIS_URL` and maintain short-TTL presence keys `corvin:presence:{user_id}`; game helpers under `src/game/` store live append-only logs at `corvin:game:{id}:meta` (initial FEN, seat ids, stub clocks) and `corvin:game:{id}:events` (JSON Move / ServerNote entries with monotonic `seq`), reconstruct position FEN by replaying Move UCIs through [chess-core](https://github.com/corvin-chess/chess-core), and apply seated legal moves via `apply_move` (SAN + FEN + PROTOCOL reject codes; clocks echoed unchanged until CV-163); auth routes under `/api/v1/auth/email/otp/*` and `/api/v1/auth/passkey/*` mint HttpOnly `corvin_session` cookies (`SameSite=Lax`, `Secure` on https) and return a synchronizer `csrf_token` plus messaging flags (prior sessions for that user are revoked); `GET /api/v1/auth/session` resolves the cookie to `{user_id,username,csrf_token,dm_enabled,public_chat_enabled}`; `POST /api/v1/auth/logout` requires matching `X-CSRF-Token` and clears the cookie; `GET|PATCH /api/v1/account/messaging` reads or updates those flags (CSRF on PATCH; estimated age under 18 or missing `birth_year` cannot enable DMs or public chat). Planned surfaces: auth ticket on the socket, lobby and matchmaking, WS Move dispatch, clock tick/flag, ratings, tournaments, DM delivery (CV-281 uses `assert_can_send_dm`), and the wider HTTP API. Intended dependencies: PostgreSQL for durable data, Redis for live games and presence, chess-core for legality, and optional analysis via [engine](https://github.com/corvin-chess/engine). Fair-play review hooks are planned to consume the public [fairplay](https://github.com/corvin-chess/fairplay) library; private thresholds stay out of this repository. Outbound SMTP is not wired yet; OTP delivery uses a log sink unless tests inject a capture mailer.

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

Library helpers `touch_presence` / `is_online` / `clear_presence` use keys `corvin:presence:{user_id}` with a short TTL. Live game helpers `create_game` / `append_move_event` / `apply_move` / `reconstruct_fen` use `corvin:game:{id}:meta` and `corvin:game:{id}:events` (replay reconstructs FEN; `apply_move` rejects illegal and wrong-turn moves).

WebSocket handshake (no Postgres required for the socket itself):

```bash
# After cargo run, open ws://127.0.0.1:3000/ws and send:
# {"typ":"Hello","v":1,"client":"web","locale":"en"}
# Expect Welcome, then a server Ping; reply with Pong using the same nonce.
```

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=DEVELOPMENT" alt="Development" /></p>

Edit `src/lib.rs` (router and `AppState`), `src/ws/` (PROTOCOL Hello/Welcome/Ping), `src/auth/` (email OTP, passkeys, sessions, mailer), `src/safety/` (age defaults and DM gate), `src/account/` (messaging prefs), `src/db.rs` (pool and migrate up/down), `src/redis_store.rs` (Redis connect and presence keys), `src/game/` (append-only game event log, FEN replay, legal `apply_move`), `src/main.rs` (bind, graceful shutdown, migrate CLI), and SQL under `migrations/`. Local Postgres and Redis from [infra](https://github.com/corvin-chess/infra) supply `DATABASE_URL` / `REDIS_URL`. Secrets and fair-play private configuration never live in this repository.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=TESTING" alt="Testing" /></p>

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node scripts/verify-readme.mjs
```

`tests/health.rs` asserts `/health` returns 200 with body `ok\n` and unknown paths return 404. `tests/ws_hello.rs` binds a local listener and contracts `Hello`/`Welcome`/`Ping`/`Pong` plus `PROTO_VERSION` against PROTOCOL.md (no Postgres). `tests/migrations.rs` asserts paired up/down SQL and, when `DATABASE_URL` is set, applies migrate up/down against Postgres including `passkeys` / `email_otps` and messaging flags (CI runs Postgres 16). `tests/presence.rs` asserts presence key shape and, when `REDIS_URL` is set, PING plus touch/clear/TTL expiry against Redis (CI runs Redis 7). `tests/game_event_log.rs` asserts FEN replay for a known opening line always-on and, when `REDIS_URL` is set, create/append/reconstruct against Redis. `tests/move_apply.rs` asserts PROTOCOL `MoveApplied` / `MoveRejected` JSON shapes always-on and, when `REDIS_URL` is set, legal apply plus `ILLEGAL_MOVE` / `NOT_YOUR_TURN` / `AUTH` / `GAME_GONE` rejects. `tests/auth_email_otp.rs` and `tests/auth_passkey.rs` exercise register/login e2e when `DATABASE_URL` is set (capture mailer; SoftPasskey). `tests/session_csrf.rs` asserts cookie attribute matrix always-on and, when `DATABASE_URL` is set, CSRF-bound logout plus session probe security tests. `tests/age_defaults.rs` asserts under-18 / missing birth year default DMs off, adults can toggle via `/api/v1/account/messaging`, and minors cannot enable messaging. Broader WS move dispatch, clocks, and Compose integration checks land with later tasks.

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><img src="https://readme-typing-svg.demolab.com?font=IBM+Plex+Mono&weight=700&size=18&color=8FA3D9&center=true&vCenter=true&width=420&height=40&duration=1&pause=1000000&letterSpacing=6px&lines=ARCHITECTURE" alt="Architecture" /></p>

Binary crate `corvin-server`: `src/lib.rs` owns the Axum router and `AppState`; `src/ws/` implements PROTOCOL connection lifecycle (`Hello`/`Welcome`/`Ping`/`Pong` on `/ws`); `src/auth/` implements PROTOCOL auth paths with `webauthn-rs` and hashed email OTPs; `src/safety/` owns age-based messaging defaults and `assert_can_send_dm`; `src/account/` serves messaging preference GET/PATCH; `src/db.rs` embeds reversible sqlx migrations and exposes `migrate_up` / `migrate_down`; `src/redis_store.rs` opens a Redis connection manager from `REDIS_URL` and owns presence key helpers; `src/game/` owns live game meta, append-only Move event lists in Redis, FEN reconstruction, and legal `apply_move` (PROTOCOL `MoveApplied` / `MoveRejected`) via `corvin-chess-core`; `src/main.rs` binds `HOST`/`PORT` and serves with graceful shutdown, or runs `migrate up|down` when those args are passed. Planned modules: `src/api`, `src/match`, `src/clock`, `src/rating`, and WS Move dispatch on `src/ws`. Server is authoritative for game state and clocks; clients reconcile to protocol events. See organization ADRs and [website](https://github.com/corvin-chess/website) once docs are published.

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
| Play protocol / WS | Planned |
| Matchmaking | Planned |
| Implementation | Planned |

<img src="https://capsule-render.vercel.app/api?type=rect&height=3&color=0:0b0d12,35:8fa3d9,70:c9a227,100:0b0d12" width="100%" alt="" />

<br/>

<p align="center"><sub><i>A move is real only when the server has written it down.</i></sub></p>

## License

This repository is licensed under AGPL-3.0-or-later.

<img src="https://capsule-render.vercel.app/api?type=waving&section=footer&height=110&color=0:0b0d12,55:161b2b,100:2f3d63" alt="" width="100%" />
