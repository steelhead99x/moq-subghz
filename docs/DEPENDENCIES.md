# Dependencies

The dependencies that need a decision on record. Everything else comes from crates.io at the
versions in `Cargo.lock`, which CI uses unchanged (`--locked`).

## rind (git, pinned)

* What: the AEAD behind object sealing (`seal` feature of `moq-subghz-wire`), from
  [kixel-ai/rind](https://github.com/kixel-ai/rind). It is pure Rust, and armv7 uses its portable
  backend, so the Pi builds need no C toolchain for it.
* Pin: `rev = "d2dc35bd796aaadc42df56aee0f1526cdee94b66"` in `moq-subghz-wire/Cargo.toml`, the same
  commit as in `Cargo.lock`. The pin is the moq-subghz decision; the upstream is kixel-ai's `main`.
* Why a git rev: rind has no published release, so there is no version to depend on. A rev pins
  exactly the code that was reviewed and built on the Pi, and no `cargo update` can move it.
* Consequences: crates.io yanks and RustSec advisories do not reach it, so nobody is told if it
  gets one. It sits behind `seal` only; `cargo test -p moq-subghz-wire --no-default-features`
  builds without it.
* Licence: Apache-2.0, ISC and MIT-0 parts (a Graviola derivative), compatible with this
  project's MIT OR Apache-2.0.
* To move the pin: change the `rev`, run `cargo update -p rind`, re-run the wire tests including
  the golden sealed vectors, and say so in the PR (a changed AEAD can change what goes on the air).

## Toolchains

`rust-toolchain.toml` pins 1.90.0, the version CI builds the workspace with.
`moq-subghz-publish/rust-toolchain.toml` pins 1.100.0 because moq.dev's crates need Rust 1.91 or
newer. The CI `clippy-stable` job lints the workspace on current stable and does not block a merge.
