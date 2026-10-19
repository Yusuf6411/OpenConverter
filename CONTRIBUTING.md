# Contributing

Read docs/MASTER_PROMPT.md and docs/PROGRESS.md first. Before opening a PR run:

    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo xtask check-deps

Use Conventional Commits. Do not add dependencies that implement formats (see §5); every new dependency needs a line in docs/DEPENDENCIES.md.
