# mrn: notes for coding agents

FileBot-style media renamer for Linux, in Rust. Read these first:
- `docs/DEVELOPMENT.md`: status, what's verified, known issues, roadmap, how to resume.
- `docs/ARCHITECTURE.md`: module map, pipeline, parser/matcher/template rules.
- `README.md`: user-facing usage.

## Commands

```sh
cargo build
cargo test --workspace                                   # must stay green
cargo clippy --workspace --all-targets -- -D warnings    # must stay clean
cargo run -p mrn-cli -- rename <dir> -o <lib> -n         # dry run
```

## Rules

- Running `mrn` by hand writes to the real `~/.local/share/mrn/history.db` and
  `~/.cache/mrn`. For experiments, set `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_CACHE_HOME`
  to a temp dir, as `crates/mrn-cli/tests/cli.rs` does.
- Parser change: add the file name to `crates/mrn-core/tests/fixtures/filenames.toml` first.
- New behaviour goes in `mrn-core` (usually `pipeline.rs`). Keep `mrn-cli` thin so a GUI can reuse it.
- Every disk change goes through `history` (start_op → finish_op/fail_op) so undo stays complete.
- Providers must keep `with_base_url()` for wiremock tests. Never put API keys in errors or cache keys.
- The repo is on an NTFS (fuseblk) mount, so builds are slow.
