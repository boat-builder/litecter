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

**Companion doc.** This is the *producer* half — how CI builds, signs, notarizes
and publishes a release. The *consumer* half — how a running copy of Litecter
notices that release and installs it in one click — is
[docs/auto-update.md](auto-update.md).

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

### 2. Generate the updater keypair

Separate chain, separate key: Apple's cert makes the *installer* open without a
Gatekeeper warning; this minisign key makes the *in-app update* trustworthy.
The keypair is **per app** — do not copy doklin's. A leak in one project should
not become a code-execution vector in another.

```bash
mkdir -p ~/.tauri
cd app && npx tauri signer generate -w ~/.tauri/litecter.key
```

It prompts for a password twice and writes two files, each a **single line of
base64 with no trailing newline**:

| File | Contents | Goes to |
|---|---|---|
| `~/.tauri/litecter.key` | private key | the `TAURI_SIGNING_PRIVATE_KEY` secret, verbatim |
| `~/.tauri/litecter.key.pub` | public key | `plugins.updater.pubkey` in `app/src-tauri/tauri.conf.json`, committed |

**The password must be non-empty and must not begin or end with whitespace.**
GitHub rejects an empty secret and the release job's `-n` check would fail on
one; leading/trailing spaces survive in the key file but get silently eaten by
shell tokenisation on the way into a secret, which fails the signing step with
a confusing error.

Stamp the public key in and verify it decodes before committing:

```bash
sed -i '' "s|REPLACE_ME_WITH_THE_MINISIGN_PUBLIC_KEY|$(cat ~/.tauri/litecter.key.pub)|" \
  app/src-tauri/tauri.conf.json
jq -r .plugins.updater.pubkey app/src-tauri/tauri.conf.json | base64 -d
# -> untrusted comment: minisign public key: <ID>
#    RW...
```

Then the two secrets — from a file and from stdin, never through a shell
variable, for the tokenisation reason above:

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY -R boat-builder/litecter < ~/.tauri/litecter.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD -R boat-builder/litecter   # prompts
gh secret list -R boat-builder/litecter                                     # expect 8
```

Finally, fold both into the gitignored root `.env` alongside the Apple
credentials, so local signed builds work and the generated key files can be
deleted:

```bash
printf "TAURI_SIGNING_PRIVATE_KEY='%s'\n" "$(cat ~/.tauri/litecter.key)" >> .env
printf "TAURI_SIGNING_PRIVATE_KEY_PASSWORD='%s'\n" 'the-password' >> .env
rm ~/.tauri/litecter.key ~/.tauri/litecter.key.pub
```

Single-quote every value in this file. That is what makes `set -a; . ./.env`
safe for credentials with spaces, parentheses or trailing whitespace — the
corruption mode called out in the warning above. Pick a password with no
single quote in it, since one would terminate the quoting.

(`TAURI_SIGNING_PRIVATE_KEY` also accepts a *path* under the alternate name
`TAURI_SIGNING_PRIVATE_KEY_PATH`, if you would rather keep the key file and not
inline it.)

### 3. Verify the chain before you rely on it

```bash
./scripts/verify-updater-key.sh
```

Reads the key from `.env` (or the environment), confirms the CLI can sign with
it, and — the check that earns the script — signs a scratch file and compares
the key id in the signature against the one in the committed pubkey. A
mismatched pair is invisible everywhere else: it builds green, publishes green,
and then fails signature verification on every user's machine at install time.

> **Back the private key and its password up outside both CI and this
> checkout.** A GitHub secret cannot be read back and `.env` is one `rm -rf` or
> one dead laptop from gone, so a fresh clone plus a lost machine means the key
> is unrecoverable — and installed copies only accept artifacts signed by the
> pubkey they were built with, so losing it strands every existing install on
> its current version permanently. Put the key and password in a password
> manager the moment you generate them; the only other recovery is telling
> users to download a fresh DMG by hand.

### 4. There is no step 4

In particular, the `Settings → Actions → General → Workflow permissions` radio
does **not** need changing. This repo is on GitHub's default of `read`, and
`release.yml` declares `permissions: contents: write` at the workflow level,
which overrides that default for its own run — so the bump job can push its
commit and tag regardless. (doklin releases from the same `read` default.) The
repo setting only supplies the baseline for workflows that declare nothing.

The Apple half of this setup is already done — `v0.1.0` and `v0.1.1` shipped
signed and notarized. The updater half (step 2) gates only the releases cut
after it lands.

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

1. **Checks the secrets first**, before the expensive build, and that the
   committed updater pubkey is not still a placeholder.
2. Checks out the exact tag the bump job created.
3. Builds with `npm run tauri build -- --target aarch64-apple-darwin`.
   Tauri does the whole signing story from environment variables: imports the
   cert into an ephemeral keychain, codesigns with hardened runtime, submits
   the `.app` to `notarytool`, waits, staples the ticket, then signs the `.dmg`.
   `bundle.createUpdaterArtifacts` also makes it emit `Litecter.app.tar.gz` and
   a detached `.sig`, signed with `TAURI_SIGNING_PRIVATE_KEY`.
4. Verifies the result rather than trusting it (see below).
5. Notarizes and staples the **`.dmg` itself** — Tauri only notarizes the
   `.app`, and a stapled DMG is what lets a fresh download pass Gatekeeper with
   no network round-trip.
6. Writes `latest.json`, inlining the signature and pinning the artifact URL to
   this tag.
7. Publishes the GitHub Release, marked latest. `make_latest` is what makes the
   stable `releases/latest/download/latest.json` endpoint resolve to this build.

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
| updater key id, signature vs. shipped pubkey | a `TAURI_SIGNING_PRIVATE_KEY` that is not the pair of the committed pubkey — an update every install would reject |

There is no entitlements file. Litecter needs none: it is not sandboxed, and
the headless browser it drives is spawned as a separate process, which hardened
runtime does not restrict.

## Release assets

Five assets per release:

| Asset | Purpose |
|---|---|
| `Litecter-<version>-macos-arm64.dmg` | versioned installer, for archival |
| `Litecter-macos-arm64.dmg` | byte-identical copy under a stable name |
| `SHA256SUMS` | checksums for both DMGs |
| `Litecter.app.tar.gz` | the bundle the in-app updater downloads and swaps in |
| `latest.json` | the manifest installed copies poll |

The two updater assets are integrity-checked by the minisign signature inlined
in `latest.json`, not by `SHA256SUMS`.

The stable-name copy is made **after** stapling, so it carries a valid ticket
too. It gives a permanent download URL that never needs updating:

```
https://github.com/boat-builder/litecter/releases/latest/download/Litecter-macos-arm64.dmg
```

Apple Silicon only. Intel is not built and not supported.

## The auto-updater

Users do not re-download to upgrade: a running copy polls `latest.json`, and
**Settings → Updates** offers a single *Update to vX.Y.Z & Restart* button.
The client half — state machine, UX rules, config, porting checklist — is
[docs/auto-update.md](auto-update.md). Only three of this pipeline's outputs are
load-bearing for it:

| Pipeline step | Why the updater needs it |
|---|---|
| The `bump` job stamps one version into five files, asserting after each | The version the app *reports* and the version the manifest *advertises* must be the same number, or the semver compare misfires |
| `TAURI_SIGNING_PRIVATE_KEY` is set during `tauri build` | Produces the `.sig` — without it, `createUpdaterArtifacts` fails the build rather than shipping an unsigned artifact |
| The publish step writes `latest.json` and marks the release `make_latest` | That is what makes the stable `releases/latest/download/latest.json` endpoint resolve to this build |

The public key in `tauri.conf.json` and the private key in the secret must be a
pair, or every update fails signature verification.

## Failure recovery

| Symptom | Fix |
|---|---|
| Notarization exceeds the 6h job ceiling | "Re-run failed jobs" — it reuses the bump job's tag and resubmits. Never re-push. |
| `tag vX.Y.Z already exists` | Delete the tag, or raise `version` in `app/src-tauri/tauri.conf.json`. |
| `missing signing secrets: …` | The named secret is unset or empty on this repo. |
| Bump job can't push | The `permissions: contents: write` block was dropped from `release.yml`, or an org policy caps the token below it. |
| Users see "app is damaged" | Notarization or stapling silently failed; check the verify step, and reproduce with `spctl -a -vvv -t install <dmg>`. |
| Version stamped in some files but not others | The stamp step's own assertions failed the job — a file's `version` line no longer matches the expected shape. |
| `pubkey … is still a placeholder` | One-time setup step 2 was never done. Generate the keypair, commit the public half. |
| Build fails on a missing signing key with `createUpdaterArtifacts` on | `TAURI_SIGNING_PRIVATE_KEY` is unset or its password is wrong. This failure is deliberate — it beats publishing a release nobody can update to. |
| Release is green but nobody updates | See [auto-update.md § Gotchas](auto-update.md#gotchas-worth-knowing-up-front) — usually version drift or a `platforms` key that doesn't match the build target. |

## Testing changes to the pipeline

`workflow_dispatch` runs the full path manually, but it still cuts a real
release. To check that the build and bundle paths are right without publishing,
build locally — it produces the same layout the workflow expects:

```bash
set -a; . ./.env; set +a          # Apple six + the two TAURI_SIGNING_* vars
cd app && npm run tauri build -- --target aarch64-apple-darwin
```

The Tauri CLI does **not** auto-load the repo's `.env`, so it has to be sourced
explicitly. That is safe as long as every value in it is single-quoted — see
[one-time setup step 2](#2-generate-the-updater-keypair).

`createUpdaterArtifacts` makes the bundler **refuse to build** without the
signing key rather than emit an unsigned artifact, so a local bundle needs the
key exported as above. To compile without bundling at all — the usual inner
loop — use `npm run tauri build -- --no-bundle`, which needs no key.
