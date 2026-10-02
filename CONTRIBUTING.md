# Contributing

Thanks for helping build this. A few ground rules keep the project healthy.

## Workflow

1. Open an issue (or pick one) before large changes so the design can be
   discussed first. Architecture decisions are recorded in `docs/adr/`.
2. Branch from `main`, keep PRs focused, and make sure `cargo fmt`,
   `cargo clippy -- -D warnings` and `cargo test` pass locally.
3. Add a test for every behaviour change. Pixel-level code gets a numeric test
   with an explicit expected value, not a snapshot.

## Code guidelines

- Engine crates (`tiles`, `doc`, `render`, `io`, `core`) never depend on a
  window, a GPU context or the UI toolkit.
- Every edit to a document goes through a `Command`. Do not mutate the
  document from the UI directly.
- Keep the CPU compositing path as the reference implementation; the GPU path
  must produce the same pixels within tolerance, and a test should check it.
- Parsers for file formats must be fuzzable (`cargo fuzz`) and never panic on
  bad input.

## Contributor License Agreement

The project is GPL-3.0-or-later. To keep the option of dual licensing and
signed app-store builds, contributors grant the project maintainers a licence
to relicense their contributions. A CLA bot will ask you to sign on your
first pull request. The CLA text lives in `CLA.md` (to be added before the
first external contribution).

## Communication

Design discussion happens in GitHub issues and the project chat. Be kind;
assume good faith.
