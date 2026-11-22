# Contributing

Thanks for looking. This is a hobby project, so reviews may be slow.

## Build and test

You need Rust 1.90 or newer and ffmpeg. `rust-toolchain.toml` selects 1.90.0 (what CI uses) if you use rustup.

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

The radio driver and the firmware are separate crates:

```sh
(cd vendor/sx1276 && cargo test)
(cd firmware/catwan-fw && cargo build --release)   # needs the thumbv6m-none-eabi target
```

You don't need radios to try a change. The README's quick start runs the whole pipeline through
a file.

## Sending a change

* Keep a pull request to one thing, and say how you checked it.
* If a change alters what goes on the air (framing, FEC, pacing, encryption), how loss is counted,
  or the firmware image, say so. The published results were measured on real radios, and such a
  change means they need measuring again.
* Say plainly what you could not run, such as a radio test.
* Never commit a key file, a serial log from your own bench with host names in it, or a flash dump.

Planned work is in [quest/](quest/README.md), one Markdown file per pull request. Maintainers and
their agents use the [Quest](https://github.com/moq-dev/quest) workflow described in
[AGENTS.md](AGENTS.md); outside contributors don't need to.

## License

Contributions are accepted under the project's license: MIT or Apache 2.0, at the user's choice.
