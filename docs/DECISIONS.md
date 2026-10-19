# Decisions

## ADR-0001: Rust engine, own codecs, Tauri shell
Context: OpenConverter 1.x is pure Python. 2.x needs native speed, process isolation and a desktop app.
Decision: a Rust workspace with our own format code and only vetted infrastructure crates (MASTER_PROMPT §2, §5); Tauri v2 for the desktop shell.
Consequences: 1.x is retired from main and stays available at tag v1.0.0. Releases restart at 2.0.0.

## ADR-0002: Work lands by pull request, not direct push to main
Context: MASTER_PROMPT §28 says push to main after each milestone; this thread develops on a feature branch.
Decision: each milestone goes through a PR from the working branch.
Consequences: the push gate runs before each PR update; CI is checked on the PR.
