#!/usr/bin/env node
/**
 * CV-199: assert server Dockerfile + entrypoint exist and stay deploy-safe.
 * Structural checks only; image build runs in infra Compose CI.
 */
import { readFileSync, existsSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(__dirname, "..")

const fail = (msg) => {
  console.error(`FAIL: ${msg}`)
  process.exitCode = 1
}
const ok = (msg) => console.log(`OK: ${msg}`)

const dockerfilePath = join(repoRoot, "Dockerfile")
const entryPath = join(repoRoot, "docker", "entrypoint.sh")

if (!existsSync(dockerfilePath)) {
  fail("missing Dockerfile")
  process.exit(1)
}

const dockerfile = readFileSync(dockerfilePath, "utf8")

if (/[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/u.test(dockerfile)) {
  fail("emoji found in Dockerfile")
} else {
  ok("Dockerfile has no emoji")
}

if (!/FROM\s+rust:/i.test(dockerfile)) {
  fail("Dockerfile must use an official rust build image")
} else {
  ok("rust build stage present")
}

if (!/cargo build --locked --release/i.test(dockerfile)) {
  fail("Dockerfile must cargo build --locked --release")
} else {
  ok("locked release build")
}

if (/Stockfish|Leela|lc0|lichess|chess\.com/i.test(dockerfile)) {
  fail("Dockerfile must not name third-party chess products")
} else {
  ok("no third-party chess product names")
}

if (/BEGIN (RSA |OPENSSH |EC )?PRIVATE KEY|ghp_[A-Za-z0-9]{36,}/i.test(dockerfile)) {
  fail("Dockerfile looks like it embeds a secret")
} else {
  ok("no embedded secret smell")
}

if (!existsSync(entryPath)) {
  fail("missing docker/entrypoint.sh")
} else {
  const entry = readFileSync(entryPath, "utf8")
  if (!/migrate up/i.test(entry)) {
    fail("entrypoint must run migrate up before serve")
  }
  if (!/corvin-server/i.test(entry)) {
    fail("entrypoint must exec corvin-server")
  }
  if (!/#!\s*\/bin\/(sh|bash)/.test(entry)) {
    fail("entrypoint needs a POSIX shebang")
  }
  if (!process.exitCode) ok("entrypoint migrates then serves")
}

if (!/EXPOSE\s+3000/i.test(dockerfile)) {
  fail("Dockerfile must EXPOSE 3000 (matches Compose health)")
} else {
  ok("EXPOSE 3000")
}

if (!/entrypoint\.sh/i.test(dockerfile)) {
  fail("Dockerfile must install docker/entrypoint.sh")
} else {
  ok("entrypoint wired in Dockerfile")
}

if (!process.exitCode) {
  console.log("All CV-199 server Dockerfile checks passed.")
}
