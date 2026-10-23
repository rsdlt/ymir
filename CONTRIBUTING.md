# Keeping the paths clear

## Checks

CI runs these on every push. Run them before you commit:

```sh
cargo fmt --check
cargo clippy --locked --all-targets
cargo test --locked
cargo deny check    # licenses and known vulnerabilities, per deny.toml
```

After upgrading dependencies (`cargo update`, or `cargo upgrade` for major versions), run
`cargo deny check` again.

## Measuring

Some tests read your own `~/.claude/projects` and print sizes and timings, never content.
They're ignored by default; run them in release mode:

```sh
cargo test --release -- --ignored --nocapture
```

- `real_index_speed`: discovering and indexing every session.
- `real_transcripts`: opening every session; entry numbers must agree with the index.
- `filter_speed`: refiltering keystroke by keystroke.

Put what they print, measured like for like, in the commit message and in
[docs/plan.md](docs/plan.md).

## Privacy

Sessions hold secrets. Test fixtures are synthetic and written by hand
(`tests/fixtures/`, `src/testutil.rs`); never copy a real session into the repository, not
even a line. ymir makes no network calls; keep it that way.

## Releases

1. List the changes under "Unreleased" in [CHANGELOG.md](CHANGELOG.md), then date them under
   the new version.
2. Bump `version` in `Cargo.toml` following [SemVer](https://semver.org/).
3. Commit "Release x.y.z" and tag it `vx.y.z`.
4. Push the commit and the tag, then run `cargo publish`.

## Design

[docs/plan.md](docs/plan.md) records how ymir works and why: the session format as found,
the decisions, the search semantics and what's left for later.
