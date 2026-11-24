#!/usr/bin/env node
/**
 * Assert server README matches README_STYLE repository template rules (CV-012/CV-130).
 */
import { readFileSync, existsSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(__dirname, "..")
const workspaceRoot = resolve(repoRoot, "..")
const readmePath = join(repoRoot, "README.md")
const licensePath = join(repoRoot, "LICENSE")
const stylePath = join(workspaceRoot, "README_STYLE.md")
const reposPath = join(workspaceRoot, "REPOS.md")

const REPO = "server"
const SKILL = "rust"
const LICENSE_ID = "AGPL-3.0-or-later"
const CLOSING =
  "A move is real only when the server has written it down."

const PLANNED_AREAS =
  /^(Implementation)$/i
const BUILT_AREAS =
  /^(README|Axum scaffold|Postgres migrations|Redis presence|Auth|Session cookies \/ CSRF|Age-appropriate defaults|WS Hello\/Welcome\/Ping|Game event log|Move apply|Clocks|Lag compensation|Offers|Draw claims|Matchmaking|Ratings|Finished game persist|Protocol contract fixtures|Play protocol \/ WS)$/i

const fail = (msg) => {
  console.error(`FAIL: ${msg}`)
  process.exitCode = 1
}

const ok = (msg) => console.log(`OK: ${msg}`)

if (!existsSync(readmePath)) {
  fail(`missing ${readmePath}`)
  process.exit(1)
}

const readme = readFileSync(readmePath, "utf8")

const emojiRe = /[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/u
if (emojiRe.test(readme)) {
  fail("emoji found in README")
} else {
  ok("no emoji")
}

const requiredSections = [
  "OVERVIEW",
  "GETTING+STARTED",
  "DEVELOPMENT",
  "TESTING",
  "ARCHITECTURE",
  "STATUS",
]
for (const section of requiredSections) {
  if (!readme.includes(`lines=${section}`)) {
    fail(`missing typing SVG section title: ${section}`)
  }
}
if (!process.exitCode) ok("required section typing titles present")

const sectionAlts = [
  "Overview",
  "Getting Started",
  "Development",
  "Testing",
  "Architecture",
  "Status",
]
for (const alt of sectionAlts) {
  if (!readme.includes(`alt="${alt}"`)) {
    fail(`missing section alt text: ${alt}`)
  }
}
if (!process.exitCode) ok("section alt texts present")

if (!readme.includes(`https://github.com/corvin-chess/${REPO}`)) {
  fail(`missing self link https://github.com/corvin-chess/${REPO}`)
} else {
  ok("self repository link present")
}

if (!readme.includes(`i=${SKILL}&theme=dark`)) {
  fail(`skill icon must be i=${SKILL}&theme=dark`)
} else {
  ok(`skill icon i=${SKILL} theme=dark`)
}

if (!readme.includes("i=github&theme=dark")) {
  fail("GitHub skill icon with theme=dark missing")
} else {
  ok("GitHub skill icon present")
}

// Repository README template uses these DESIGN.md palette hexes (F8FAFC is org-profile buttons only).
const palette = ["0b0d12", "161b2b", "2f3d63", "8fa3d9", "c9a227", "e6e9f2"]
for (const hex of palette) {
  if (!readme.toLowerCase().includes(hex.toLowerCase())) {
    fail(`palette hex missing: ${hex}`)
  }
}
if (!process.exitCode) ok("DESIGN.md palette hexes present")

if (!readme.includes("capsule-render.vercel.app")) {
  fail("capsule-render header/footer missing")
} else {
  ok("capsule-render present")
}

if (!readme.includes("readme-typing-svg.demolab.com")) {
  fail("typing SVG missing")
} else {
  ok("typing SVG present")
}

// Header typing: repo name then one-line description; spaces as +, no raw spaces in lines=
const headerTypingMatch = readme.match(
  /readme-typing-svg\.demolab\.com\?[^"']*lines=([^"'&]+)/
)
if (!headerTypingMatch) {
  fail("could not parse header typing lines=")
} else {
  const linesParam = headerTypingMatch[1]
  if (/\s/.test(linesParam)) {
    fail("typing SVG lines= contains raw whitespace; encode spaces as +")
  }
  if (!linesParam.startsWith(`${REPO};`)) {
    fail(`typing SVG lines= must start with ${REPO};`)
  }
  ok(`typing SVG lines= encoded and starts with ${REPO};`)
}

if (!readme.includes(`<i>${CLOSING}</i>`)) {
  fail("closing sentence missing or altered")
} else {
  ok("closing sentence matches README_STYLE server example")
}

if (!readme.includes(LICENSE_ID) && !readme.includes("AGPL-3.0-or-later")) {
  fail(`license section must state ${LICENSE_ID}`)
} else {
  ok(`license named ${LICENSE_ID}`)
}

const statusTableMatch = readme.match(
  /\|\s*Area\s*\|\s*State\s*\|[\s\S]*?(?=<img src="https:\/\/capsule-render)/
)
if (!statusTableMatch) {
  fail("STATUS table missing or malformed")
} else {
  const table = statusTableMatch[0]
  const stateCells = [...table.matchAll(/\|\s*([^|\n]+)\s*\|\s*([^|\n]+)\s*\|/g)]
  let sawReadmeBuilt = false
  let sawAxumBuilt = false
  let sawMigrationsBuilt = false
  let sawRedisPresenceBuilt = false
  let sawAuthBuilt = false
  let sawSessionCsrfBuilt = false
  let sawAgeDefaultsBuilt = false
  let sawWsHelloBuilt = false
  let sawGameEventLogBuilt = false
  let sawMoveApplyBuilt = false
  let sawClocksBuilt = false
  let sawLagBuilt = false
  let sawOffersBuilt = false
  let sawDrawClaimsBuilt = false
  let sawMatchmakingBuilt = false
  let sawRatingsBuilt = false
  let sawFinishedPersistBuilt = false
  let sawProtocolFixturesBuilt = false
  let sawPlayProtocolBuilt = false
  let sawImplPlanned = false
  for (const [, areaRaw, stateRaw] of stateCells) {
    const area = areaRaw.trim()
    const state = stateRaw.trim()
    if (/^area$/i.test(area) && /^state$/i.test(state)) continue
    if (/^-+$/.test(area)) continue
    if (!/^(Planned|Built)$/i.test(state)) {
      fail(`STATUS row "${area}" must be Planned or Built; got "${state}"`)
      continue
    }
    if (BUILT_AREAS.test(area) && !/^Built$/i.test(state)) {
      fail(`STATUS row "${area}" must be Built; got "${state}"`)
    }
    if (/^README$/i.test(area) && /^Built$/i.test(state)) {
      sawReadmeBuilt = true
    }
    if (/^Axum scaffold$/i.test(area) && /^Built$/i.test(state)) {
      sawAxumBuilt = true
    }
    if (/^Postgres migrations$/i.test(area) && /^Built$/i.test(state)) {
      sawMigrationsBuilt = true
    }
    if (/^Redis presence$/i.test(area) && /^Built$/i.test(state)) {
      sawRedisPresenceBuilt = true
    }
    if (/^Auth$/i.test(area) && /^Built$/i.test(state)) {
      sawAuthBuilt = true
    }
    if (/^Session cookies \/ CSRF$/i.test(area) && /^Built$/i.test(state)) {
      sawSessionCsrfBuilt = true
    }
    if (/^Age-appropriate defaults$/i.test(area) && /^Built$/i.test(state)) {
      sawAgeDefaultsBuilt = true
    }
    if (/^WS Hello\/Welcome\/Ping$/i.test(area) && /^Built$/i.test(state)) {
      sawWsHelloBuilt = true
    }
    if (/^Game event log$/i.test(area) && /^Built$/i.test(state)) {
      sawGameEventLogBuilt = true
    }
    if (/^Move apply$/i.test(area) && /^Built$/i.test(state)) {
      sawMoveApplyBuilt = true
    }
    if (/^Clocks$/i.test(area) && /^Built$/i.test(state)) {
      sawClocksBuilt = true
    }
    if (/^Lag compensation$/i.test(area) && /^Built$/i.test(state)) {
      sawLagBuilt = true
    }
    if (/^Offers$/i.test(area) && /^Built$/i.test(state)) {
      sawOffersBuilt = true
    }
    if (/^Draw claims$/i.test(area) && /^Built$/i.test(state)) {
      sawDrawClaimsBuilt = true
    }
    if (/^Matchmaking$/i.test(area) && /^Built$/i.test(state)) {
      sawMatchmakingBuilt = true
    }
    if (/^Ratings$/i.test(area) && /^Built$/i.test(state)) {
      sawRatingsBuilt = true
    }
    if (/^Finished game persist$/i.test(area) && /^Built$/i.test(state)) {
      sawFinishedPersistBuilt = true
    }
    if (/^Protocol contract fixtures$/i.test(area) && /^Built$/i.test(state)) {
      sawProtocolFixturesBuilt = true
    }
    if (/^Play protocol \/ WS$/i.test(area) && /^Built$/i.test(state)) {
      sawPlayProtocolBuilt = true
    }
    if (PLANNED_AREAS.test(area) && !/^Planned$/i.test(state)) {
      fail(`STATUS row "${area}" must stay Planned until code exists; got "${state}"`)
    }
    if (/^Implementation$/i.test(area) && /^Planned$/i.test(state)) {
      sawImplPlanned = true
    }
  }
  if (!sawReadmeBuilt) fail('STATUS must include "| README | Built |"')
  if (!sawAxumBuilt) fail('STATUS must include "| Axum scaffold | Built |"')
  if (!sawMigrationsBuilt) fail('STATUS must include "| Postgres migrations | Built |"')
  if (!sawRedisPresenceBuilt) fail('STATUS must include "| Redis presence | Built |"')
  if (!sawAuthBuilt) fail('STATUS must include "| Auth | Built |"')
  if (!sawSessionCsrfBuilt) {
    fail('STATUS must include "| Session cookies / CSRF | Built |"')
  }
  if (!sawAgeDefaultsBuilt) {
    fail('STATUS must include "| Age-appropriate defaults | Built |"')
  }
  if (!sawWsHelloBuilt) {
    fail('STATUS must include "| WS Hello/Welcome/Ping | Built |"')
  }
  if (!sawGameEventLogBuilt) {
    fail('STATUS must include "| Game event log | Built |"')
  }
  if (!sawMoveApplyBuilt) {
    fail('STATUS must include "| Move apply | Built |"')
  }
  if (!sawClocksBuilt) {
    fail('STATUS must include "| Clocks | Built |"')
  }
  if (!sawLagBuilt) {
    fail('STATUS must include "| Lag compensation | Built |"')
  }
  if (!sawOffersBuilt) {
    fail('STATUS must include "| Offers | Built |"')
  }
  if (!sawDrawClaimsBuilt) {
    fail('STATUS must include "| Draw claims | Built |"')
  }
  if (!sawMatchmakingBuilt) {
    fail('STATUS must include "| Matchmaking | Built |"')
  }
  if (!sawRatingsBuilt) {
    fail('STATUS must include "| Ratings | Built |"')
  }
  if (!sawFinishedPersistBuilt) {
    fail('STATUS must include "| Finished game persist | Built |"')
  }
  if (!sawProtocolFixturesBuilt) {
    fail('STATUS must include "| Protocol contract fixtures | Built |"')
  }
  if (!sawPlayProtocolBuilt) {
    fail('STATUS must include "| Play protocol / WS | Built |"')
  }
  if (!sawImplPlanned) fail('STATUS must include "| Implementation | Planned |"')
  if (!process.exitCode) ok("STATUS rows use Planned/Built correctly")
}

const claimedBuilt = /\b(Shipped|Production|Live at|users online|ELO|nps\s*=)/i
if (claimedBuilt.test(readme)) {
  fail("unverified performance or production claims found")
} else {
  ok("no unverified strength/production claims")
}

if (!readme.includes("cargo run") || !readme.includes("cargo test --locked")) {
  fail("GETTING STARTED / TESTING must document cargo run and cargo test --locked")
} else {
  ok("cargo run and cargo test --locked documented")
}

if (!readme.includes("/health") || !readme.includes("HOST") || !readme.includes("PORT")) {
  fail("README must document /health and HOST/PORT bind overrides")
} else {
  ok("/health and HOST/PORT documented")
}

if (
  !readme.includes("DATABASE_URL") ||
  !readme.includes("migrate up") ||
  !readme.includes("migrate down")
) {
  fail("README must document DATABASE_URL and migrate up/down")
} else {
  ok("DATABASE_URL and migrate up/down documented")
}

if (
  !readme.includes("REDIS_URL") ||
  !readme.includes("corvin:presence:") ||
  !readme.includes("touch_presence")
) {
  fail("README must document REDIS_URL and presence key helpers")
} else {
  ok("REDIS_URL and presence helpers documented")
}

const redisStoreRs = join(repoRoot, "src", "redis_store.rs")
const presenceTest = join(repoRoot, "tests", "presence.rs")
if (!existsSync(redisStoreRs) || !existsSync(presenceTest)) {
  fail("Redis presence sources missing (src/redis_store.rs, tests/presence.rs)")
} else {
  ok("Redis presence sources present")
}

const authMod = join(repoRoot, "src", "auth", "mod.rs")
const authOtpTest = join(repoRoot, "tests", "auth_email_otp.rs")
const authPasskeyTest = join(repoRoot, "tests", "auth_passkey.rs")
const authMigrationUp = join(
  repoRoot,
  "migrations",
  "20261124120000_passkeys_email_otps.up.sql"
)
if (
  !existsSync(authMod) ||
  !existsSync(authOtpTest) ||
  !existsSync(authPasskeyTest) ||
  !existsSync(authMigrationUp)
) {
  fail("Auth sources missing (src/auth, auth tests, passkeys/email_otps migration)")
} else {
  ok("Auth sources present")
}

if (
  !readme.includes("/api/v1/auth") ||
  !readme.includes("passkey") ||
  !readme.includes("OTP") ||
  !readme.includes("CORVIN_RP_ORIGIN")
) {
  fail("README must document /api/v1/auth, passkey/OTP, and CORVIN_RP_ORIGIN")
} else {
  ok("Auth endpoints and RP env documented")
}

if (
  !readme.includes("/api/v1/auth/session") ||
  !readme.includes("/api/v1/auth/logout") ||
  !readme.includes("X-CSRF-Token") ||
  !readme.includes("csrf_token") ||
  !readme.includes("corvin_session")
) {
  fail(
    "README must document session/logout routes, csrf_token, X-CSRF-Token, and corvin_session"
  )
} else {
  ok("Session cookie and CSRF surface documented")
}

const csrfRs = join(repoRoot, "src", "auth", "csrf.rs")
const sessionCsrfTest = join(repoRoot, "tests", "session_csrf.rs")
if (!existsSync(csrfRs) || !existsSync(sessionCsrfTest)) {
  fail("CSRF sources missing (src/auth/csrf.rs, tests/session_csrf.rs)")
} else {
  ok("CSRF sources present")
}

const safetyRs = join(repoRoot, "src", "safety", "mod.rs")
const accountMessagingRs = join(repoRoot, "src", "account", "messaging.rs")
const ageDefaultsTest = join(repoRoot, "tests", "age_defaults.rs")
const ageDefaultsUp = join(
  repoRoot,
  "migrations",
  "20261124150000_age_defaults.up.sql"
)
if (
  !existsSync(safetyRs) ||
  !existsSync(accountMessagingRs) ||
  !existsSync(ageDefaultsTest) ||
  !existsSync(ageDefaultsUp)
) {
  fail(
    "Age-defaults sources missing (src/safety, account/messaging, tests/age_defaults, migration)"
  )
} else {
  ok("Age-appropriate defaults sources present")
}

if (
  !readme.includes("/api/v1/account/messaging") ||
  !readme.includes("dm_enabled") ||
  !readme.includes("public_chat_enabled")
) {
  fail(
    "README must document /api/v1/account/messaging and dm_enabled/public_chat_enabled"
  )
} else {
  ok("Messaging prefs surface documented")
}

const wsMod = join(repoRoot, "src", "ws", "mod.rs")
const wsPlayRs = join(repoRoot, "src", "ws", "play.rs")
const wsHubRs = join(repoRoot, "src", "ws", "hub.rs")
const wsHelloTest = join(repoRoot, "tests", "ws_hello.rs")
const wsPlayTest = join(repoRoot, "tests", "ws_play.rs")
if (!existsSync(wsMod) || !existsSync(wsHelloTest)) {
  fail("WS handshake sources missing (src/ws/mod.rs, tests/ws_hello.rs)")
} else {
  ok("WS Hello/Welcome/Ping sources present")
}
if (!existsSync(wsPlayRs) || !existsSync(wsHubRs) || !existsSync(wsPlayTest)) {
  fail("WS play sources missing (src/ws/play.rs, hub.rs, tests/ws_play.rs)")
} else {
  ok("WS play wire sources present")
}

if (
  !readme.includes("/ws") ||
  !readme.includes("Hello") ||
  !readme.includes("Welcome") ||
  !readme.includes("Ping")
) {
  fail("README must document /ws Hello/Welcome/Ping handshake")
} else {
  ok("WS handshake surface documented")
}

const gameMod = join(repoRoot, "src", "game", "mod.rs")
const gameEventLogTest = join(repoRoot, "tests", "game_event_log.rs")
if (!existsSync(gameMod) || !existsSync(gameEventLogTest)) {
  fail("Game event log sources missing (src/game/mod.rs, tests/game_event_log.rs)")
} else {
  ok("Game event log sources present")
}

if (
  !readme.includes("corvin:game:") ||
  !readme.includes("reconstruct_fen") ||
  !readme.includes("append_move_event") ||
  !readme.includes("game_event_log")
) {
  fail(
    "README must document corvin:game: keys, reconstruct_fen, append_move_event, and game_event_log tests"
  )
} else {
  ok("Game event log surface documented")
}

const applyRs = join(repoRoot, "src", "game", "apply.rs")
const moveApplyTest = join(repoRoot, "tests", "move_apply.rs")
if (!existsSync(applyRs) || !existsSync(moveApplyTest)) {
  fail("Move apply sources missing (src/game/apply.rs, tests/move_apply.rs)")
} else {
  ok("Move apply sources present")
}

if (
  !readme.includes("apply_move") ||
  !readme.includes("MoveApplied") ||
  !readme.includes("MoveRejected") ||
  !readme.includes("move_apply") ||
  !readme.includes("ILLEGAL_MOVE")
) {
  fail(
    "README must document apply_move, MoveApplied/MoveRejected, ILLEGAL_MOVE, and move_apply tests"
  )
} else {
  ok("Move apply surface documented")
}

const clockRs = join(repoRoot, "src", "game", "clock.rs")
const clocksTest = join(repoRoot, "tests", "clocks.rs")
if (!existsSync(clockRs) || !existsSync(clocksTest)) {
  fail("Clocks sources missing (src/game/clock.rs, tests/clocks.rs)")
} else {
  ok("Clocks sources present")
}

if (
  !readme.includes("claim_flag") ||
  !readme.includes("ClockSync") ||
  !readme.includes("increment") ||
  !readme.includes("clocks") ||
  !readme.includes("Fischer")
) {
  fail(
    "README must document claim_flag, ClockSync, Fischer increment, and clocks tests"
  )
} else {
  ok("Clocks surface documented")
}

const lagRs = join(repoRoot, "src", "game", "lag.rs")
const lagTest = join(repoRoot, "tests", "lag_quota.rs")
if (!existsSync(lagRs) || !existsSync(lagTest)) {
  fail("Lag compensation sources missing (src/game/lag.rs, tests/lag_quota.rs)")
} else {
  ok("Lag compensation sources present")
}

if (
  !readme.includes("lag_comp_ms") ||
  !readme.includes("apply_move_with_rtt") ||
  !readme.includes("lag_quota") ||
  !readme.includes("LagTracker")
) {
  fail(
    "README must document lag_comp_ms, apply_move_with_rtt, lag_quota tests, and LagTracker"
  )
} else {
  ok("Lag compensation surface documented")
}

const offersRs = join(repoRoot, "src", "game", "offers.rs")
const offersTest = join(repoRoot, "tests", "offers.rs")
if (!existsSync(offersRs) || !existsSync(offersTest)) {
  fail("Offers sources missing (src/game/offers.rs, tests/offers.rs)")
} else {
  ok("Offers sources present")
}

if (
  !readme.includes("offer_draw") ||
  !readme.includes("OfferState") ||
  !readme.includes("ABORT_DENIED") ||
  !readme.includes("OFFER_INVALID") ||
  !readme.includes("offers")
) {
  fail(
    "README must document offer_draw, OfferState, OFFER_INVALID/ABORT_DENIED, and offers tests"
  )
} else {
  ok("Offers surface documented")
}

const claimsRs = join(repoRoot, "src", "game", "claims.rs")
const claimsTest = join(repoRoot, "tests", "draw_claims.rs")
if (!existsSync(claimsRs) || !existsSync(claimsTest)) {
  fail("Draw claims sources missing (src/game/claims.rs, tests/draw_claims.rs)")
} else {
  ok("Draw claims sources present")
}

if (
  !readme.includes("claim_draw") ||
  !readme.includes("CLAIM_DENIED") ||
  !readme.includes("threefold") ||
  !readme.includes("fivefold") ||
  !readme.includes("draw_claims")
) {
  fail(
    "README must document claim_draw, CLAIM_DENIED, threefold/fivefold, and draw_claims tests"
  )
} else {
  ok("Draw claims surface documented")
}

const matchMod = join(repoRoot, "src", "matchmaking", "mod.rs")
const matchTest = join(repoRoot, "tests", "matchmaking.rs")
const challengeUp = join(
  repoRoot,
  "migrations",
  "20261124180000_challenges.up.sql"
)
if (!existsSync(matchMod) || !existsSync(matchTest) || !existsSync(challengeUp)) {
  fail(
    "Matchmaking sources missing (src/matchmaking/, tests/matchmaking.rs, challenges migration)"
  )
} else {
  ok("Matchmaking sources present")
}

if (
  !readme.includes("quick_pair") ||
  !readme.includes("GameStart") ||
  !readme.includes("challenge_create_link") ||
  !readme.includes("matchmaking")
) {
  fail(
    "README must document quick_pair, GameStart, challenge_create_link, and matchmaking tests"
  )
} else {
  ok("Matchmaking surface documented")
}

const ratingMod = join(repoRoot, "src", "rating", "mod.rs")
const glickoRs = join(repoRoot, "src", "rating", "glicko2.rs")
const ratingTest = join(repoRoot, "tests", "glicko2.rs")
const ratingsTest = join(repoRoot, "tests", "ratings.rs")
const ratingsUp = join(repoRoot, "migrations", "20261124200000_ratings.up.sql")
if (
  !existsSync(ratingMod) ||
  !existsSync(glickoRs) ||
  !existsSync(ratingTest) ||
  !existsSync(ratingsTest) ||
  !existsSync(ratingsUp)
) {
  fail(
    "Ratings sources missing (src/rating/, tests/glicko2.rs, tests/ratings.rs, ratings migration)"
  )
} else {
  ok("Ratings sources present")
}

if (
  !readme.includes("apply_rated_finish") ||
  !readme.includes("Glicko-2") ||
  !readme.includes("rating_diff") ||
  !readme.includes("rating_updates") ||
  !(readme.includes("glicko2") && readme.includes("ratings"))
) {
  fail(
    "README must document apply_rated_finish, Glicko-2, rating_diff, rating_updates, and glicko2/ratings tests"
  )
} else {
  ok("Ratings surface documented")
}

const persistRs = join(repoRoot, "src", "game", "persist.rs")
const pgnRs = join(repoRoot, "src", "game", "pgn.rs")
const persistTest = join(repoRoot, "tests", "persist_game.rs")
const gamesUp = join(repoRoot, "migrations", "20261124210000_games.up.sql")
const apiGames = join(repoRoot, "src", "api", "games.rs")
if (
  !existsSync(persistRs) ||
  !existsSync(pgnRs) ||
  !existsSync(persistTest) ||
  !existsSync(gamesUp) ||
  !existsSync(apiGames)
) {
  fail(
    "Finished game persist sources missing (persist.rs, pgn.rs, persist_game.rs, games migration, api/games.rs)"
  )
} else {
  ok("Finished game persist sources present")
}

if (
  !readme.includes("persist_finished_game") ||
  !readme.includes("export_pgn") ||
  !readme.includes("/api/v1/games/") ||
  !readme.includes("game_events") ||
  !readme.includes("persist_game")
) {
  fail(
    "README must document persist_finished_game, export_pgn, /api/v1/games/, game_events, and persist_game tests"
  )
} else {
  ok("Finished game persist surface documented")
}

const protocolContractTest = join(repoRoot, "tests", "protocol_contract.rs")
const protocolManifest = join(repoRoot, "tests", "fixtures", "protocol", "manifest.json")
const protocolMoveApplied = join(
  repoRoot,
  "tests",
  "fixtures",
  "protocol",
  "move_applied.json"
)
if (
  !existsSync(protocolContractTest) ||
  !existsSync(protocolManifest) ||
  !existsSync(protocolMoveApplied)
) {
  fail(
    "Protocol contract fixtures missing (tests/protocol_contract.rs, tests/fixtures/protocol/manifest.json, move_applied.json)"
  )
} else {
  ok("Protocol contract fixture sources present")
}

if (
  !readme.includes("protocol_contract") ||
  !readme.includes("tests/fixtures/protocol") ||
  !readme.includes("--test protocol_contract")
) {
  fail(
    "README must document protocol_contract tests, fixtures path, and cargo test --test protocol_contract"
  )
} else {
  ok("Protocol contract surface documented")
}

if (!existsSync(join(repoRoot, "src", "lib.rs")) || !existsSync(join(repoRoot, "src", "main.rs"))) {
  fail("Axum scaffold source missing (src/lib.rs, src/main.rs)")
} else {
  ok("Axum scaffold source present")
}

const migrationsDir = join(repoRoot, "migrations")
const upSql = join(migrationsDir, "20261123120000_users_sessions.up.sql")
const downSql = join(migrationsDir, "20261123120000_users_sessions.down.sql")
const dbRs = join(repoRoot, "src", "db.rs")
if (!existsSync(upSql) || !existsSync(downSql) || !existsSync(dbRs)) {
  fail("Postgres migrations missing (migrations/*.up.sql, *.down.sql, src/db.rs)")
} else {
  ok("Postgres migration sources present")
}

if (!existsSync(join(repoRoot, "Cargo.toml"))) {
  fail("missing Cargo.toml")
} else {
  const cargo = readFileSync(join(repoRoot, "Cargo.toml"), "utf8")
  if (!cargo.includes('name = "corvin-server"')) {
    fail('Cargo.toml must name package corvin-server')
  } else if (!cargo.includes("axum")) {
    fail("Cargo.toml must depend on axum")
  } else if (!cargo.includes("sqlx")) {
    fail("Cargo.toml must depend on sqlx for migrations")
  } else if (!cargo.includes("redis")) {
    fail("Cargo.toml must depend on redis for presence")
  } else if (!cargo.includes("corvin-chess-core")) {
    fail("Cargo.toml must depend on corvin-chess-core for FEN replay")
  } else if (!/"ws"/.test(cargo) && !cargo.includes('"ws"')) {
    fail('Cargo.toml axum must enable the "ws" feature')
  } else {
    ok("Cargo.toml names corvin-server with axum (ws), sqlx, redis, and chess-core")
  }
}

const linkRe = /https:\/\/github\.com\/corvin-chess\/([A-Za-z0-9._-]+)/g
const linked = new Set()
let m
while ((m = linkRe.exec(readme)) !== null) {
  linked.add(m[1].replace(/\.git$/i, ""))
}

const knownRepos = new Set([
  "web",
  "server",
  "chess-core",
  "engine",
  "engine-train",
  "worker",
  "fairplay",
  "puzzles",
  "explorer",
  "broadcast",
  "design-system",
  "assets",
  "mobile",
  "sdk",
  "bots",
  "translations",
  "infra",
  "website",
  ".github",
])

for (const name of linked) {
  if (!knownRepos.has(name)) {
    fail(`link to unknown repo: ${name}`)
  }
}
if (!linked.has(REPO)) {
  fail(`missing link to ${REPO}`)
}
if (!process.exitCode) ok("all github.com/corvin-chess links are in REPOS set")

if (existsSync(reposPath)) {
  const reposDoc = readFileSync(reposPath, "utf8")
  if (!reposDoc.includes(`### ${REPO}`)) {
    fail(`REPOS.md missing ### ${REPO}`)
  } else {
    ok("REPOS.md documents server")
  }
}

if (!existsSync(licensePath)) {
  fail("missing LICENSE")
} else {
  const license = readFileSync(licensePath, "utf8")
  if (!/GNU AFFERO GENERAL PUBLIC LICENSE/i.test(license)) {
    fail("LICENSE must contain GNU Affero General Public License title")
  } else if (!/Version 3/i.test(license)) {
    fail("LICENSE must be AGPL Version 3")
  } else if (license.length < 20000) {
    fail("LICENSE looks truncated; expect full AGPL-3.0 text")
  } else {
    ok("LICENSE present with full AGPL-3.0 text")
  }
  if (emojiRe.test(license)) {
    fail("emoji found in LICENSE")
  }
}

if (existsSync(stylePath)) {
  const style = readFileSync(stylePath, "utf8")
  if (!style.includes("| server | `rust` |")) {
    fail("README_STYLE skill table missing server/rust")
  } else {
    ok("README_STYLE lists server skill icon rust")
  }
  if (!style.includes(CLOSING)) {
    fail("README_STYLE missing server closing sentence example")
  } else {
    ok("closing sentence listed in README_STYLE")
  }
}

const requiredHeaderBits = [
  'alt="Corvin"',
  `alt="${REPO}"`,
  "section=footer",
  "## License",
]
for (const bit of requiredHeaderBits) {
  if (!readme.includes(bit)) {
    fail(`missing required README fragment: ${bit}`)
  }
}
if (!process.exitCode) ok("header, footer, license heading present")

if (!process.exitCode) {
  console.log("All server README checks passed.")
}
