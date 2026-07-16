# cosmon

This PyPI package **reserves the `cosmon` name** on PyPI. It contains no
runnable code.

[Cosmon](https://docs.noogram.org) is a stateless CLI that gives AI agents a
persistent identity, a typed lifecycle, and crash-recovery — distributed as a
single binary named **`cs`**, not as a Python package.

## Install the `cs` binary

Prebuilt binaries for Linux and macOS are attached to every
[GitHub Release](https://github.com/noogram/cosmon/releases).

From source (requires a Rust toolchain):

```sh
cargo install --git https://github.com/noogram/cosmon cosmon-cli
```

See <https://docs.noogram.org> for documentation.

---

*Why a placeholder?* Reserving `cosmon` on PyPI is cheap insurance against
name-squatting once the project is public. Unlike oxymake — whose audience
lives in pip/conda and gets a working thin launcher — cosmon's audience lives
in the terminal and in Rust, so this entry is a pure name-hold. If a real
Python launcher is ever wanted (one that downloads the `cs` binary on first
run), it replaces this metadata-only build in a future minor.
