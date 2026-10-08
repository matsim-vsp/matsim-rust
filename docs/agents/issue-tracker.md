# Issue tracker: GitHub

Issues and specs for this repo live in GitHub Issues for `titipakorn-th/matsim-rust`. Use the `gh` CLI for all operations.

## Conventions

- Create issues with `gh issue create`.
- Read issues with `gh issue view <number> --comments`, including labels.
- List issues with `gh issue list` and filters appropriate to the task.
- Comment, label, and close issues with `gh issue comment`, `gh issue edit`, and `gh issue close`.
- Infer the repository from `git remote -v`; `gh` resolves it automatically from this clone.

## Pull requests as a triage surface

**PRs as a request surface: no.** Set this to `yes` only if external PRs should be treated as feature requests.

When enabled, use the corresponding `gh pr` commands. GitHub shares number space between issues and PRs, so resolve a bare `#42` with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says “publish to the issue tracker”

Create a GitHub issue.

## When a skill says “fetch the relevant ticket”

Run `gh issue view <number> --comments`.
