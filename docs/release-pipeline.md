# Release pipeline

How Litecter gets from a commit on `main` to a signed, notarized `.dmg` that
opens on a stranger's Mac without a Gatekeeper warning.

Two workflows do the work:

| Workflow | Trigger | Job |
|---|---|---|
| [`ci.yml`](../.github/workflows/ci.yml) | pull requests | clippy + tests, frontend typecheck |
| [`release.yml`](../.github/workflows/release.yml) | push to `main`, or manual dispatch | bump the version, build/sign/notarize, publish the release |

**Every push to `main` cuts a release.** There is no separate "publish" step to
remember and no unsigned fallback — if the signing secrets are missing the job
fails before it builds anything.

## One-time setup

Nothing below is in the repo; the pipeline is inert until it is done.

### 1. Add the six Apple secrets

These are the same six the other apps on this Apple Developer account use, with
the same names. GitHub secrets cannot be read back, so copy the *values* from
wherever they are originally kept — not from another repo's settings page.

```bash
for s in MACOS_CERTIFICATE MACOS_CERTIFICATE_PWD MACOS_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do gh secret set "$s" -R boat-builder/litecter; done
```

> **Do not pipe these in by `source`-ing a `.env`.** `MACOS_SIGNING_IDENTITY`
> holds `Developer ID Application: Name (TEAMID)` — unquoted, that is a shell
> syntax error, and `source` then aborts leaving every later key unset.
> `gh secret set` accepts empty stdin and prints a checkmark anyway, so the
> result is several silently blank secrets. Sourcing corrupts values even when
> it parses: `MACOS_CERTIFICATE_PWD` ends in a space that is part of the
> password, and shell tokenisation drops trailing whitespace — yielding a
> secret that looks fine and fails the `.p12` import. Set these from a literal
> parser or paste them into the web UI. The `Require signing secrets` step
> catches blanks, but only after you push, and it cannot catch a wrong value.

| Secret | What it is |
|---|---|
| `MACOS_CERTIFICATE` | Developer ID Application `.p12`, base64-encoded |
| `MACOS_CERTIFICATE_PWD` | Export password for that `.p12` |
| `MACOS_SIGNING_IDENTITY` | Full identity string, e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | Developer account email |
| `APPLE_PASSWORD` | App-specific password for notarization (**not** the account password) |
| `APPLE_TEAM_ID` | 10-character team identifier |

No `KEYCHAIN_PWD` is needed — Tauri creates its own ephemeral keychain on the
runner and tears it down afterwards.

### 2. There is no step 2

In particular, the `Settings → Actions → General → Workflow permissions` radio
does **not** need changing. This repo is on GitHub's default of `read`, and
`release.yml` declares `permissions: contents: write` at the workflow level,
which overrides that default for its own run — so the bump job can push its
commit and tag regardless. (doklin releases from the same `read` default.) The
repo setting only supplies the baseline for workflows that declare nothing.

The first push to `main` after the secrets exist releases `v0.1.0`
(the version currently committed in `tauri.conf.json`), and every push after
that patch-bumps it.

## How a release runs

### Job 1 — `bump` (ubuntu)

1. Reads `version` from `app/src-tauri/tauri.conf.json` and the newest `v*` tag.
2. Computes the next version: **patch-bump the newest tag**, unless the
   committed file version is *higher* than that tag, in which case the file
   version wins verbatim. That is how you cut a minor or major release — raise
   `version` in `app/src-tauri/tauri.conf.json` in your own commit and push.
   With no tags at all, the committed version ships as-is.
3. Fails loudly if the computed tag already exists, rather than clobbering it.
4. Stamps the version into five files:
   - `app/package.json`
   - `app/package-lock.json` (two keys: top level and `packages.""`)
   - `app/src-tauri/tauri.conf.json`
   - `app/src-tauri/Cargo.toml`
   - `Cargo.lock` — the **workspace** lock at the repo root, only the
     `litecter-app` entry. `litecter-core` and `litecter` keep their own
     versions; the app version is the release version.
5. Commits as `chore: release vX.Y.Z [skip ci]`, tags, pushes both.

Stamping uses `awk` on the single matching line rather than `jq`, so the
hand-maintained JSON/TOML formatting survives. (`package-lock.json` is
generated, so it gets `jq` — which addresses both of its version keys by name.)

The bump commit is pushed with `GITHUB_TOKEN`, and GitHub never triggers
workflows from `GITHUB_TOKEN` pushes — so the bump cannot re-trigger the
release. That is also why the build runs in the *same* workflow rather than a
separate on-tag one: a `GITHUB_TOKEN`-pushed tag would never fire it.

Concurrent pushes are serialized by a `release-main` concurrency group, so a
second push waits and then bumps on top of the first rather than racing it.

### Job 2 — `build-release` (macos-15, arm64)

1. **Checks the secrets first**, before the expensive build.
2. Checks out the exact tag the bump job created.
3. Builds with `npm run tauri build -- --target aarch64-apple-darwin`.
   Tauri does the whole signing story from environment variables: imports the
   cert into an ephemeral keychain, codesigns with hardened runtime, submits
   the `.app` to `notarytool`, waits, staples the ticket, then signs the `.dmg`.
4. Verifies the result rather than trusting it (see below).
5. Notarizes and staples the **`.dmg` itself** — Tauri only notarizes the
   `.app`, and a stapled DMG is what lets a fresh download pass Gatekeeper with
   no network round-trip.
6. Publishes the GitHub Release, marked latest.

Note that `app/src-tauri` is a member of the **root** Cargo workspace, so build
output lands in `target/` at the repo root — not `app/src-tauri/target/`. The
workflow's paths and the `rust-cache` workspace both reflect that.

### Verification gates

Signing failures are quiet by nature: a build that skipped notarization looks
exactly like one that didn't until a user downloads it. So the workflow asserts:

| Check | Catches |
|---|---|
| `codesign --verify --deep --strict` | unsigned or broken-signature bundles |
| `xcrun stapler validate` (app **and** dmg) | Tauri silently skipping notarization, e.g. an env var it didn't recognize |
| `spctl -a -vvv -t exec` | Gatekeeper's own verdict, as a first-launch user's Mac sees it |
| `lipo -archs` per binary | an `x86_64` slice sneaking into an arm64-only build |

There is no entitlements file. Litecter needs none: it is not sandboxed, and
the headless browser it drives is spawned as a separate process, which hardened
runtime does not restrict.

## Release assets

Three assets per release:

| Asset | Purpose |
|---|---|
| `Litecter-<version>-macos-arm64.dmg` | versioned installer, for archival |
| `Litecter-macos-arm64.dmg` | byte-identical copy under a stable name |
| `SHA256SUMS` | checksums for both |

The stable-name copy is made **after** stapling, so it carries a valid ticket
too. It gives a permanent download URL that never needs updating:

```
https://github.com/boat-builder/litecter/releases/latest/download/Litecter-macos-arm64.dmg
```

Apple Silicon only. Intel is not built and not supported.

## No auto-updater (yet)

Deliberately out of scope for now: users update by downloading a new DMG. That
is why there are no `TAURI_SIGNING_*` secrets, no `createUpdaterArtifacts`, and
no `latest.json` here, even though the sibling doklin pipeline has all three.

To add it later:

1. `npx @tauri-apps/cli signer generate -w ~/.tauri/litecter.key` — keep the
   private key out of the repo, set it as `TAURI_SIGNING_PRIVATE_KEY` and its
   password as `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
2. In `tauri.conf.json`: `bundle.createUpdaterArtifacts: true`, plus a
   `plugins.updater` block with the matching **public** key and an endpoint of
   `https://github.com/boat-builder/litecter/releases/latest/download/latest.json`.
3. Add `tauri-plugin-updater` and `tauri-plugin-process`, register both, and
   declare the `updater:default` capability.
4. In `release.yml`: require the two new secrets, pass them to the build step,
   and add a step that publishes the build's `.app.tar.gz` + `.sig` and writes
   `latest.json` with the signature and a **version-tagged** URL (not `latest`).

The public key in the config and the private key in the secret must be a pair,
or every update fails signature verification.

## Failure recovery

| Symptom | Fix |
|---|---|
| Notarization exceeds the 6h job ceiling | "Re-run failed jobs" — it reuses the bump job's tag and resubmits. Never re-push. |
| `tag vX.Y.Z already exists` | Delete the tag, or raise `version` in `app/src-tauri/tauri.conf.json`. |
| `missing signing secrets: …` | The named secret is unset or empty on this repo. |
| Bump job can't push | The `permissions: contents: write` block was dropped from `release.yml`, or an org policy caps the token below it. |
| Users see "app is damaged" | Notarization or stapling silently failed; check the verify step, and reproduce with `spctl -a -vvv -t install <dmg>`. |
| Version stamped in some files but not others | The stamp step's own assertions failed the job — a file's `version` line no longer matches the expected shape. |

## Testing changes to the pipeline

`workflow_dispatch` runs the full path manually, but it still cuts a real
release. To check that the build and bundle paths are right without publishing,
build unsigned locally — it produces the same layout the workflow expects:

```bash
cd app && npm run tauri build -- --target aarch64-apple-darwin
```
