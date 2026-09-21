# omp-launchpad

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

## Requirements

- OMP 18.1.19 or newer
- Bun
- Rust 1.88 or newer when running from a source checkout without a packaged native binary
- Git for merge-proposal checkout and push operations
- An authenticated `lpcli` session for private resources and Launchpad write operations

Public Launchpad resources can be read without credentials.

## Install

Install the extension from GitHub:

```sh
omp plugin install github:goulinkh/omp-launchpad
```

Start a new OMP process after installation so it loads the extension.

For authenticated or write access, install and log in with `lpcli`:

```sh
cargo install --git https://github.com/canonical/lpcli --locked lpcli
lpcli login
lpcli status
```

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
```

Bug and merge-proposal views include comments by default. Add `?comments=0` to omit them or `?limit=<n>` to limit them to at most 20. Non-Launchpad paths continue to use OMP's native `read` implementation.

## Tools

| Tool              | Purpose                                                                                                             |
| ----------------- | ------------------------------------------------------------------------------------------------------------------- |
| `read`            | Read individual Launchpad resources and merge-proposal diffs through `lp://` URLs.                                  |
| `launchpad`       | View repositories, read repository files, and search bugs or merge proposals.                                       |
| `launchpad_write` | Create bugs or merge proposals, comment or review, change proposal status, and check out or push proposal branches. |

The dedicated tools expose these operations:

- `launchpad`: `resource_view`, `repo_view`, `file_read`, `search_bugs`, `search_merge_proposals`
- `launchpad_write`: `bug_create`, `merge_proposal_create`, `comment`, `set_merge_proposal_status`, `merge_proposal_checkout`, `merge_proposal_push`

Example prompts:

```text
Use launchpad to search bugs for target ubuntu with query installer and limit 5.

Use launchpad to search merge proposals in repository launchpad with status "Needs review" and limit 5.

Check out lp://~owner/project/+git/repository/+merge/123 into /tmp/proposal-123.
```

A merge-proposal checkout clones the source branch and adds the target repository as an `upstream` remote when it differs from the source. A push sends the current branch to `origin`; `force_with_lease` is available when explicitly requested.

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
