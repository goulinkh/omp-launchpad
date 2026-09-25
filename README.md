<h1>
  <img src="assets/launchpad.svg" alt="Launchpad logo" title="Launchpad" width="20">
  omp-launchpad
</h1>

`omp-launchpad` is a [Launchpad](https://launchpad.net/) integration for Oh My Pi (OMP). It lets agents read Launchpad URLs through OMP's standard `read` tool and adds dedicated tools for searching, editing, and working with Launchpad bugs, Git repositories, and merge proposals.

## Capabilities

### Browse Launchpad

Read Launchpad resources through `lp://` URLs, including bugs, projects, Git repositories, merge proposals, and proposal diffs.

### Search and update

Search project bugs and repository merge proposals. Authenticated operations can create bugs and merge proposals, add comments or reviews, and update proposal status.

### Local Git workflows

- Check out a merge proposal into a local directory.
- Configure the proposal target as an `upstream` remote when it differs from the source repository.
- Push the checked-out proposal branch to `origin`.
- Optionally push with `--force-with-lease`.

### OMP integration

- Handles Launchpad URLs through OMP's standard `read` tool.
- Delegates non-Launchpad reads back to OMP.
- Supports authenticated `lpcli` credentials and anonymous public access.
- Supports production, staging, and custom Launchpad API endpoints.
- Provides native release binaries for Linux, macOS, and Windows on x64 and arm64.
- Falls back to building with Cargo when no packaged binary is available.

## Install

Install the npm package. It contains the published Rust binaries for every
supported platform, so installation does not build repository source:

```sh
omp plugin install omp-launchpad
```

Start a new OMP process after installation so it loads the extension.

For authenticated or write access, start OMP and complete the authentication flow:

```text
/launchpad login
/launchpad status
```

*Note:* The extension calls its bundled `lpcli` library directly, so a separate `lpcli` installation is not required. The standalone `lpcli` command remains available for direct use when installed; its authentication commands share the same stored credentials.

If `lpcli` is not installed in your shell, use `/launchpad login` in OMP or
`cargo run --release -- login` from a source checkout. Missing or rejected API
credentials print a login hint with an exact terminal command that first selects
the bridge's working directory. Login requires an interactive terminal and browser.

## Quick start

Start OMP and refer to a Launchpad resource in your prompt:

```sh
omp
```

```text
Read lp://bugs/1?comments=0 and summarize the bug.
```

The `read` integration recognizes these forms:

```text
lp://bugs/<id>
lp://<project>
lp://~owner/project/+git/repository
lp://~owner/project/+git/repository/+merge/<id>
lp://~owner/project/+git/repository/+merge/<id>/diff
lp://~owner/project/+git/repository/+merge/<id>/diff/<preview-diff-id>
```

Bug and merge-proposal views include comments by default. Add `?comments=0` to omit them or `?limit=<n>` to limit them to at most 20. A merge-proposal diff defaults to the current preview diff; select a historical snapshot with the final path segment or `?preview_diff=<id>`. Non-Launchpad paths continue to use OMP's native `read` implementation.

The merge-proposal badge shares OMP's status bar after the Git segment. While
the extension is loaded, runtime-only layout overrides move all extension
statuses into that bar instead of extra lines. An explicitly disabled hook
status (`statusLine.showHookStatus: false`) remains hidden.

## Tools

| Tool              | Purpose                                                                                                                       |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `read`            | Read individual Launchpad resources and current or historical merge-proposal diffs through `lp://` URLs.                     |
| `launchpad`       | View resources, find proposals by branch, read threaded discussions and inline comments, inspect drafts and diffs, search, and map file locations. |
| `launchpad_write` | Create or update resources, update inline drafts, submit reviews, and check out or push merge-proposal branches.              |

The dedicated tools expose these operations:

- `launchpad`: `resource_view`, `repo_view`, `file_read`, `search_bugs`, `search_merge_proposals`, `merge_proposal_for_branch`, `current_merge_proposal`, `merge_proposal_discussion`, `preview_diffs`, `inline_comments`, `review_drafts`, `diff_line_map`
- `launchpad_write`: `bug_create`, `merge_proposal_create`, `comment`, `review_draft_update`, `review_submit`, `set_merge_proposal_status`, `merge_proposal_checkout`, `merge_proposal_push`

For `resource_view`, `preview_diff_id` selects that snapshot's diff. The target may be a numeric merge-proposal ID or a merge-proposal URL and does not need a `/diff` suffix.

Example prompts:

```text
Use launchpad to search bugs for target ubuntu with query installer and limit 5.

Find the merge proposal for branch `fix-login` in repository `my-project`.

Resolve the preferred merge proposal for the current Git checkout.

Show the compact review summary for merge proposal 123 from a related Launchpad checkout.

Show the structured merge-proposal discussion for lp://~owner/project/+git/repository/+merge/123.

Show the merge-proposal discussion for the current Git checkout with format both.

Show unresolved inline comments on the current diff for lp://~owner/project/+git/repository/+merge/123.

Use launchpad with op merge_proposal_discussion, comments inline, current_diff_only true, reviewer alice, and since 2026-09-01T00:00:00Z.

Use launchpad with op preview_diffs for lp://~owner/project/+git/repository/+merge/123.

Map modified file line 42 in src/main.rs to a global diff line for preview diff 456, then save an inline draft there.

Submit a review for preview diff 456 with vote Approve; include the saved inline drafts.

Check out lp://~owner/project/+git/repository/+merge/123 into /tmp/proposal-123.
```

Repository parameters accept short names, canonical paths, `lp://` identifiers,
Launchpad web URLs, HTTPS clone URLs, SSH URLs, and `git@` clone URLs. Merge
proposal targets also accept a numeric ID such as `511601` when the working
checkout has a related Launchpad remote; use the full Launchpad URL or path
when no repository context is available.

Merge-proposal creation resolves both repository inputs to their canonical
Launchpad names before looking up refs. A failed lookup identifies the source
or target repository or ref that could not be resolved.

`current_merge_proposal` inspects the working directory, current branch, and
all Git remotes. It prefers a Launchpad `origin`, otherwise the first Launchpad
remote. `merge_proposal_for_branch` and branch-based
`merge_proposal_discussion` use the same inference when repository and branch
are omitted.

Branch lookup first checks the requested source repository, then same-named and
default repositories for the same Launchpad target. It applies `status` and
`target_branch`, excludes superseded proposals unless `include_superseded` is
true or the status filter explicitly requests them, and prefers an active
proposal over a merged proposal. Within that status class it selects the newest
creation date and ID. `latest: true` instead selects the newest matching
proposal regardless of status class. Successful matches expose `selected_proposal`,
`selection_reason`, filters, candidates, and alternatives. Ambiguous
results report inspected repositories and actionable retry parameters.

An exhaustive branch lookup without a proposal matching its filters returns a
successful result with `details.found: false` and the repository, branch, and
filters. Found proposals return `details.found: true`. Repository, permission,
transport, and incomplete (capped) lookup failures remain errors; do not treat
them as evidence that no proposal exists. Explicit proposal targets that do
not exist also remain errors.

For no match, `details.repository` is canonical, `details.requested_repository`
retains the input, and `details.inspected_repositories` lists related candidates.
Discussion lookups also honor `format` when no proposal matches.

Discussion `format` accepts `summary`, `structured`, or `both`; `summary` is
the default. The compact summary reports general-comment and review counts,
current/open, outdated, and superseded inline-thread counts, current review
votes, and vote transitions. Launchpad does not expose formal inline-thread
resolution, so resolved count is reported as unavailable. `structured`
returns the complete JSON discussion without Markdown duplication; `both`
returns the compact summary followed by that JSON. The same JSON is always
available in the tool result details, including normalized identities, full
general comments, review activity, and flattened inline threads with file,
source/target line, diff line, replies, and state.

By default, discussions inspect inline threads from every preview diff.
`current_diff_only` narrows that history; `comments` accepts `all`, `general`,
or `inline`; `since` accepts an RFC 3339 timestamp; and `reviewer` matches a
Launchpad username or display name. `unresolved_only` returns current, open
threads. Stale threads are `outdated`, and non-current non-stale threads are
`superseded`.

Searches paginate up to the requested limit. Limits above 1,000 are rejected
explicitly rather than silently truncated.

A merge-proposal checkout clones the source branch and adds the target repository as an `upstream` remote when it differs from the source. A push sends the current branch to `origin`; `force_with_lease` is available when explicitly requested.

Inline draft updates and review submissions require an explicit `preview_diff_id`. The bridge re-fetches the merge proposal immediately before each write and rejects a snapshot that is no longer current or is marked stale.

`file_read` fetches through anonymous `git.launchpad.net/plain`, even when API
credentials are present. A redirect or failed response does not establish that
the repository is private: check its name, file path, and branch first. Errors
include that context. API login does not authorize the Git file endpoint;
private files require an authenticated Git checkout.

## Configuration

| Variable                    | Effect                                                                                |
| --------------------------- | ------------------------------------------------------------------------------------- |
| `OMP_LAUNCHPAD_ANONYMOUS=1` | Force anonymous access. Launchpad write operations are unavailable in this mode.      |
| `OMP_LAUNCHPAD_INSTANCE`    | Select `production` (default), `staging`, `qastaging`, or a custom hostname.          |
| `OMP_LAUNCHPAD_API_VERSION` | Select the Launchpad API version; defaults to `devel`.                                |
| `OMP_LAUNCHPAD_API_BASE`    | Override the complete Launchpad API base URL.                                         |
| `OMP_LAUNCHPAD_BINARY`      | Run a specific native bridge binary instead of the packaged binary or Cargo fallback. |

## Development

Install dependencies, run static checks, and run the Rust unit tests:

```sh
bun install --frozen-lockfile
bun run check
cargo test --locked
```

Load the working tree directly in OMP:

```sh
omp --no-extensions -e ./index.ts
```

A source checkout without a native binary invokes Cargo from OMP's current
working directory. Cargo enforces the Rust version required by the bridge and
its dependencies. If that directory selects an older toolchain, run OMP with
a compatible `RUSTUP_TOOLCHAIN` (for example `1.88.0`) or use a native binary.
The extension preserves Cargo's error and adds compiler guidance when applicable.

See [DEVELOPMENT.md](DEVELOPMENT.md) for smoke tests, release packaging, plugin linking, and bridge diagnostics.

## License

GPL-3.0-or-later.
