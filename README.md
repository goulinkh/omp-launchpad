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

Agents should use the `launchpad` and `launchpad_write` tools or the
`/launchpad login` command; probing `lpcli --help` is not a prerequisite and
may fail simply because no standalone executable is installed.

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

- `launchpad`: `resource_view`, `repo_view`, `file_read`, `search_bugs`, `search_merge_proposals`, `merge_proposal_for_branch`, `current_merge_proposal`, `merge_proposal_discussion`, `merge_proposal_bugs`, `preview_diffs`, `inline_comments`, `review_drafts`, `diff_line_map`
- `launchpad_write`: `bug_create`, `bug_edit`, `bug_task_edit`, `project_edit`, `repository_edit`, `merge_proposal_create`, `merge_proposal_edit`, `replace_merge_proposal_prerequisite`, `merge_proposal_link_bug`, `merge_proposal_unlink_bug`, `comment`, `comment_edit`, `review_draft_update`, `review_submit`, `set_merge_proposal_status`, `merge_proposal_checkout`, `merge_proposal_push`

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

Show the bugs linked to lp://~owner/project/+git/repository/+merge/123.

Link bug 456 to lp://~owner/project/+git/repository/+merge/123.

Update the status and importance of the task at lp://project/+bug/456.

Edit a comment at lp://~owner/project/+git/repository/+merge/123/comments/789.

Update the description of the repository at lp://~owner/project/+git/repository.

Update the summary of lp://project.

Create a merge proposal from feature-2 into main with prerequisite_ref feature-1, prerequisite_repository ~owner/project/+git/base, commit_message "Add the dependent feature", wait_for_index true, and wait_for_preview true.

Replace the prerequisite on lp://~owner/project/+git/repository/+merge/123 with feature-1.

Edit the commit_message on lp://~owner/project/+git/repository/+merge/123 without changing its URL.

Map modified file line 42 in src/main.rs to a global diff line for preview diff 456, then save an inline draft there.

Submit a review for preview diff 456 with vote Approve; include the saved inline drafts.

Check out lp://~owner/project/+git/repository/+merge/123 into /tmp/proposal-123.
```

Repository parameters accept short names, canonical paths, `lp://` identifiers,
Launchpad web URLs, HTTPS clone URLs, SSH URLs, and `git@` clone URLs. Merge
proposal targets also accept a numeric ID such as `511601` when the working
checkout has a related Launchpad remote; use the full Launchpad URL or path
when no repository context is available.

Merge-proposal creation resolves `source_ref`, `target_ref`, and optional
`prerequisite_ref` Git refs. Set `target_ref` to the merge destination (for
example, `main`), and `prerequisite_ref` to the formal prerequisite branch the
source builds on. `prerequisite_repository` selects its repository and requires
`prerequisite_ref`; when omitted, the prerequisite is looked up in the source
repository. Launchpad exposes the resulting read-only proposal field as
`prerequisite_git_path`; that field is not a creation parameter. Replacement
continues to use `merge_prerequisite` instead of the creation fields.

Git SSH aliases such as `~owner/project` fall back to
`~owner/project/+git/project` when Launchpad cannot resolve the short path;
other unresolved aliases report the canonical `lp://` path to supply.
Creation requires a nonblank `commit_message`; Launchpad uses it as the
proposal title. `description` remains optional and does not supply a title.
The creation result includes the canonical refs, prerequisite metadata, and
the source commit when Launchpad exposes it. Proposal reads and `preview_diffs`
show the prerequisite; preview history includes the prerequisite revision used
by Launchpad to generate the diff. Deleted files in preview diffstat are
reported by their source paths rather than Launchpad's `dev/null` placeholder.

Launchpad's Git server may expose a newly pushed ref before its API ref collection
does. A Git-visible but API-missing ref reports `ref_pending_index`;
`wait_for_index: true` waits before proposal submission for API visibility.
It cannot fix a wrong repository, inaccessible Git server, or missing ref.
`wait_for_preview: true` separately waits for Launchpad to generate
the preview diff. Optional `index_timeout_seconds` and
`preview_timeout_seconds` require their corresponding wait flag to be true and
each must be between 1 and 300 seconds. Waiting is bounded; it does not make
asynchronous Launchpad work instantaneous. Creation reports `creation_state`
and `preview_state`; a pending preview is not a failed creation.
`preview_diffs` can return `state: pending`, `diffs: []`, and `retryable: true`
until Launchpad finishes generating the diff. Retry that read rather than
creating a second proposal.

Creation checks for a recent proposal with matching source, target, prerequisite,
commit message, description, and source commit (when available) before posting.
After an ambiguous submission timeout it checks for a newly indexed match
instead of posting again; `creation_state: recovered` distinguishes recovery
from a new submission. If the outcome remains unknown, inspect the source
branch's proposals before retrying. This is not an idempotency guarantee for
older proposals or concurrently identical requests.

`merge_proposal_edit` changes `commit_message`, `description`, and/or
`reviewed_revid` on the same proposal via Launchpad's writable fields. It
cannot change the source, target, or prerequisite refs. Launchpad cannot edit
a prerequisite in place:
`replace_merge_proposal_prerequisite` creates a new proposal with the
original source and target, description, and commit message, then marks the
old proposal Superseded. This changes the URL and does not move comments or
reviews. If creation fails, the tool attempts to restore the old status and
reports if restoration also fails. Changing a merge target requires a new
proposal.

`merge_proposal_bugs` reads linked bugs through Launchpad's authenticated
collection, even for public proposals. `merge_proposal_link_bug` and
`merge_proposal_unlink_bug` change that relationship by numeric `bug_id`.
Proposal output includes source/target repository API links; `repo_view` shows
the repository's target project link. `resource_view` accepts these API links
as well as `lp://` and Launchpad web URLs.

`bug_edit` changes the bug's title, description, or tags. Status, importance,
and assignee belong to an individual bug task: read the bug to find that
task's URL, then use `bug_task_edit` on that URL. Set `unassign: true` to clear
the assignee. `project_edit` changes summary, description, reporting
guidelines, or official bug tags; the tag list replaces all existing tags.
`repository_edit` changes description or the full `refs/heads/...` default
branch. `comment_edit` targets an individual bug or proposal comment URL and
uses Launchpad's revision-preserving `editContent` operation. Launchpad
permissions still apply to every write; these typed operations do not expose
unrelated administrative or destructive API methods.

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
