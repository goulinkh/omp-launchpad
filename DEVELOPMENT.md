# Development

## Prerequisites

- Bun
- Rust 1.88 or newer
- OMP 18.1.19 or newer
- Launchpad credentials for authenticated and private-resource operations

The pinned toolchain in `rust-toolchain.toml` installs automatically through
`rustup`. Authenticate from an OMP session when write access is required:

```text
/launchpad login
/launchpad status
```

The extension uses the bundled `lpcli` library. A separately installed `lpcli` command can also manage the same credentials.

Public resources can be tested without credentials by setting:

```sh
export OMP_LAUNCHPAD_ANONYMOUS=1
```

## Native release artifacts

Changing `package.json`'s version and pushing that commit to `main` runs
`.github/workflows/release.yml`. The workflow compares the version with the
pre-push revision, then verifies the source, builds with `Cargo.lock`, and
produces native packages for:

- Linux x64 and arm64
- macOS x64 and arm64
- Windows x64 and arm64

Linux release binaries are statically linked with musl and do not depend on the
host's glibc version.

Each matrix job emits a standalone binary and a platform-specific npm tarball
named `omp-launchpad-<version>-<platform>-<architecture>`. The release job also
assembles `omp-launchpad-<version>.tgz`, which contains all six binaries. It
creates the matching `v<version>` tag and GitHub release, attaches every package
plus `SHA256SUMS`, and publishes the universal package to npm through OpenID
Connect trusted publishing. Changes to other `package.json` fields do not
release a new version. Existing tags, releases, and npm versions remain
immutable, so reusing a version fails instead of replacing published artifacts.

The universal npm tarball stores the executables at
`bin/omp-launchpad-<platform>-<architecture>[.exe]`. The TypeScript extension
selects the command in this order:

1. `OMP_LAUNCHPAD_BINARY`
2. The packaged binary matching `process.platform` and `process.arch`
3. `cargo run --release` from a source checkout

npm trusted publishing must authorize `goulinkh/omp-launchpad` and workflow
`release.yml`. Under **Allowed actions**, explicitly enable direct
`npm publish`; the default staged-publish grant does not authorize this
workflow.

Create one platform package locally after building its explicit Rust target:

```sh
cargo build --locked --release --target aarch64-apple-darwin
node scripts/package-release.mjs aarch64-apple-darwin darwin arm64
```

## Static checks

Run the TypeScript type check, Rust formatter check, and Clippy:

```sh
bun run check
```

Run the Rust unit tests separately:

```sh
cargo test
```

## Load the working tree directly

During development, load `index.ts` explicitly so OMP executes the current
working tree without installing or copying it:

```sh
omp --no-extensions -e ./index.ts
```

`--no-extensions` isolates this extension from other installed extensions.
Start a new OMP process after changing extension code because an existing
process retains the module version it loaded at startup.

The TypeScript extension prefers a matching binary in `bin/`. A working-tree
checkout does not contain generated binaries by default, so it invokes
`cargo run --release` and rebuilds source changes incrementally. To bypass
Cargo after an explicit release build, point the extension at the binary:

```sh
cargo build --release
export OMP_LAUNCHPAD_BINARY="$PWD/target/release/omp-launchpad"
```

Do not use `omp read lp://...` as the extension smoke test. The standalone
`omp read` command exercises OMP's native internal-URL router directly; it
does not execute the extension's `read` wrapper. Test through an actual agent
session instead.

## Read-path smoke tests

Test a Launchpad bug through the wrapped `read` tool:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.ts \
  'Use read exactly once on lp://bugs/1?comments=0, then print only the first heading returned.'
```

Expected heading:

```text
# Bug #1: Microsoft has a majority market share
```

Test a merge-proposal diff:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.ts \
  'Use read exactly once on lp://~finnrg/launchpad/+git/launchpad/+merge/511704/diff, then print only the first line returned.'
```

Expected prefix:

```text
diff --git
```

Verify that non-Launchpad paths still delegate to OMP's native `read`
implementation:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.ts \
  'Use read exactly once on package.json, then print only the package name.'
```

Expected output:

```text
omp-launchpad
```

## Tool smoke tests

Test the read-only `launchpad` tool:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.ts \
  'Use launchpad exactly once with op search_merge_proposals, repository launchpad, status ["Needs review"], and limit 1. Then print only the result heading.'
```

Expected heading:

```text
# Launchpad merge proposal search
```

Test repository file transport:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.ts \
  'Use launchpad exactly once with op file_read, repository launchpad, path README, and branch master. Then print only the first line returned.'
```

## Checkout smoke test

`merge_proposal_checkout` changes only local Git state, so it is the safest
`launchpad_write` operation to exercise end to end:

```sh
tmp="$(mktemp -d)"
omp -p --no-session --auto-approve --no-extensions -e ./index.ts \
  "Use launchpad_write exactly once with op merge_proposal_checkout, target lp://~enriqueesanchz/lazr.restfulclient/+git/lazr.restfulclient/+merge/500054, and directory $tmp/checkout. Then print only the first heading returned."
rm -rf "$tmp"
```

Expected heading:

```text
# Checked out Launchpad merge proposal 500054
```

Never smoke-test `bug_create`, `merge_proposal_create`, `comment`,
`set_merge_proposal_status`, or `merge_proposal_push` against production
merely to prove wiring. Use an expendable resource or a non-production
Launchpad instance:

```sh
OMP_LAUNCHPAD_INSTANCE=staging omp --no-extensions -e ./index.ts
```

`OMP_LAUNCHPAD_API_BASE` overrides the complete API base URL for a local test
server.

## Test the installed-plugin path

Link the working tree into OMP's plugin manager:

```sh
omp plugin link "$PWD"
omp plugin list --json
```

The list must show `omp-launchpad`, version `0.1.0`, with its path resolving to
this checkout. Start a fresh OMP process and repeat the read-path smoke tests
without `--no-extensions -e ./index.ts`:

```sh
omp -p --no-session --auto-approve \
  'Use read exactly once on lp://bugs/1?comments=0, then print only the first heading returned.'
```

This final pass catches interactions with other installed extensions. When
`omp-semantic-policy` evaluates all custom tools, classify the tools as:

```text
launchpad=read,launchpad_write=execute
```

The `read` wrapper itself retains OMP's native `read` classification.

To return to the published plugin after local testing, install the pinned npm
package:

```sh
omp plugin install omp-launchpad@0.1.0
```

## Bridge-only diagnosis

Execute the Rust bridge directly when extension registration succeeds but
Launchpad access fails:

```sh
printf '%s' '{"op":"resource_view","target":"lp://bugs/1?comments=0"}' |
  cargo run --quiet --release --
```

A successful response starts with:

```text
{"ok":true,"text":"# Bug #1:
```

This separates Rust, `lpcli` authentication, and Launchpad API failures from
OMP extension-loading failures.
