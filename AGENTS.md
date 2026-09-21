# Agent Instructions

Follow @CODING_STANDARD.md for coding standards and @DEVELOPMENT.md for development and verification workflows.

## Git

Use Conventional Commits:

```text
<type>(<optional scope>)<optional !>: <description>
```

- Use one of these types: `feat`, `fix`, `refactor`, `perf`, `style`, `test`, `docs`, `build`, `ops`, or `chore`.
- Keep the scope optional and project-specific; never use an issue identifier as the scope.
- Write the required description in the imperative, present tense, starting lowercase and without a trailing period.
- Add `!` before `:` for a breaking change.
- Use an optional body, separated by a blank line, to explain motivation and contrast previous behavior.
- Use an optional footer for issue references. Breaking changes require a footer beginning with `BREAKING CHANGE:`.
- Keep default Git messages for merge and revert commits.
