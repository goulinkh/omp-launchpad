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

## Tools

| Tool              | Purpose                                                                                                                       |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `read`            | Read individual Launchpad resources and current or historical merge-proposal diffs through `lp://` URLs.                     |
| `launchpad`       | View resources, find proposals by branch, read threaded discussions and inline comments, inspect drafts and diffs, search, and map file locations. |
| `launchpad_write` | Create or update resources, update inline drafts, submit reviews, and check out or push merge-proposal branches.              |

The dedicated tools expose these operations:

- `launchpad`: `resource_view`, `repo_view`, `file_read`, `search_bugs`, `search_merge_proposals`, `merge_proposal_for_branch`, `merge_proposal_discussion`, `preview_diffs`, `inline_comments`, `review_drafts`, `diff_line_map`
- `launchpad_write`: `bug_create`, `merge_proposal_create`, `comment`, `review_draft_update`, `review_submit`, `set_merge_proposal_status`, `merge_proposal_checkout`, `merge_proposal_push`

Example prompts:

```text
Use launchpad to search bugs for target ubuntu with query installer and limit 5.

Find the merge proposal for branch `fix-login` in repository `my-project`.

Show the complete merge-proposal discussion for lp://~owner/project/+git/repository/+merge/123.

Show the complete merge-proposal discussion for the current Git checkout.

Use launchpad with op preview_diffs for lp://~owner/project/+git/repository/+merge/123.

Map modified file line 42 in src/main.rs to a global diff line for preview diff 456, then save an inline draft there.

Submit a review for preview diff 456 with vote Approve; include the saved inline drafts.

Check out lp://~owner/project/+git/repository/+merge/123 into /tmp/proposal-123.
```

Repository parameters accept short names, canonical paths, `lp://` identifiers,
Launchpad web URLs, HTTPS clone URLs, SSH URLs, and `git@` clone URLs.
`merge_proposal_for_branch` and `merge_proposal_discussion` infer the repository
from the current checkout's `origin` remote and use its current branch when both
parameters are omitted. Discussion results combine general comments, review
activity, and inline threads from every preview diff, including stale status and
resolved file locations.

Searches paginate up to the requested limit. Limits above 1,000 are rejected
explicitly rather than silently truncated.

A merge-proposal checkout clones the source branch and adds the target repository as an `upstream` remote when it differs from the source. A push sends the current branch to `origin`; `force_with_lease` is available when explicitly requested.

Inline draft updates and review submissions require an explicit `preview_diff_id`. The bridge re-fetches the merge proposal immediately before each write and rejects a snapshot that is no longer current or is marked stale.

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

See [DEVELOPMENT.md](DEVELOPMENT.md) for smoke tests, release packaging, plugin linking, and bridge diagnostics.

## License

GPL-3.0-or-later.
