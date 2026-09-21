# Development

## Prerequisites

- Bun
- OMP 18.1.19 or newer
- `lp-shell` on `PATH`
- A cached Launchpad login for authenticated and private-resource tests

Confirm the external dependency before testing the extension:

```sh
lp-shell --help
lp-shell -c 'print(lp.me.name)' production devel
```

Use `-a` with `lp-shell` when testing only public resources anonymously.

## Static checks

Run the JavaScript syntax check and compile the Python bridge without writing bytecode:

```sh
bun run check
```

## Load the working tree directly

During development, load `index.js` explicitly so OMP executes the current working tree without installing or copying it:

```sh
cd /path/to/omp-launchpad
omp --no-extensions -e ./index.js
```

`--no-extensions` isolates this extension from other installed extensions. It is useful for diagnosing registration, schema, and execution failures. Remove it for the final compatibility check.

Start a new OMP process after changing extension code. An existing process retains the module version it loaded at startup unless it is explicitly reloaded.

Do not use `omp read lp://...` as the extension smoke test. The standalone `omp read` command exercises OMP's native internal-URL router directly; it does not execute the extension's `read` wrapper. Test through an actual agent session instead.

## Read-path smoke tests

Test a Launchpad bug through the wrapped `read` tool:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.js \
  'Use read exactly once on lp://bugs/1?comments=0, then print only the first heading returned.'
```

Expected heading:

```text
# Bug #1: Microsoft has a majority market share
```

Test a merge-proposal diff:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.js \
  'Use read exactly once on lp://~finnrg/launchpad/+git/launchpad/+merge/511704/diff, then print only the first line returned.'
```

Expected prefix:

```text
diff --git
```

Verify that non-Launchpad paths still delegate to OMP's native `read` implementation:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.js \
  'Use read exactly once on package.json, then print only the package name.'
```

Expected output:

```text
omp-launchpad
```

## Tool smoke tests

Test the read-only `launchpad` tool:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.js \
  'Use launchpad exactly once with op search_merge_proposals, repository launchpad, status ["Needs review"], and limit 1. Then print only the result heading.'
```

Expected heading:

```text
# Launchpad merge proposal search
```

Test repository file transport:

```sh
omp -p --no-session --auto-approve --no-extensions -e ./index.js \
  'Use launchpad exactly once with op file_read, repository launchpad, path README, and branch master. Then print only the first line returned.'
```

## Checkout smoke test

`merge_proposal_checkout` changes only local Git state, so it is the safest `launchpad_write` operation to exercise end to end:

```sh
tmp="$(mktemp -d)"
omp -p --no-session --auto-approve --no-extensions -e ./index.js \
  "Use launchpad_write exactly once with op merge_proposal_checkout, target lp://~enriqueesanchz/lazr.restfulclient/+git/lazr.restfulclient/+merge/500054, and directory $tmp/checkout. Then print only the first heading returned."
rm -rf "$tmp"
```

Expected heading:

```text
# Checked out Launchpad merge proposal 500054
```

Never smoke-test `bug_create`, `merge_proposal_create`, `comment`, `set_merge_proposal_status`, or `merge_proposal_push` against production merely to prove wiring. Use an expendable resource or a non-production Launchpad instance:

```sh
OMP_LAUNCHPAD_INSTANCE=staging omp --no-extensions -e ./index.js
```

## Test the installed-plugin path

Link the working tree into OMP's plugin manager:

```sh
omp plugin link "$PWD"
omp plugin list --json
```

The list must show `omp-launchpad`, version `0.1.0`, with its path resolving to this checkout. Start a fresh OMP process and repeat the read-path smoke tests without `--no-extensions -e ./index.js`:

```sh
omp -p --no-session --auto-approve \
  'Use read exactly once on lp://bugs/1?comments=0, then print only the first heading returned.'
```

This final pass catches interactions with other installed extensions. When `omp-semantic-policy` evaluates all custom tools, classify the tools as:

```text
launchpad=read,launchpad_write=execute
```

The `read` wrapper itself retains OMP's native `read` classification.

To return to the published plugin after local testing, rerun the dotfiles plugin installer or install the GitHub source directly:

```sh
omp plugin install github:goulinkh/omp-launchpad
```

## Bridge-only diagnosis

When extension registration succeeds but Launchpad access fails, execute the Python bridge inside `lp-shell` directly:

```sh
export OMP_LAUNCHPAD_BRIDGE="$PWD/bridge.py"
export OMP_LAUNCHPAD_REQUEST='{"op":"resource_view","target":"lp://bugs/1?comments=0"}'
lp-shell -c 'import os; globals()["lp"] = lp; exec(compile(open(os.environ["OMP_LAUNCHPAD_BRIDGE"], encoding="utf-8").read(), os.environ["OMP_LAUNCHPAD_BRIDGE"], "exec"), globals())' production devel
```

A successful response starts with:

```text
__OMP_LAUNCHPAD__{"ok": true
```

This separates `lp-shell` authentication and Launchpad API failures from OMP extension-loading failures.
