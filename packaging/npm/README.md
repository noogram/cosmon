# cosmon

This npm package **reserves the `cosmon` name** on the npm registry. It
contains no runnable code.

[Cosmon](https://docs.noogram.org) is a stateless CLI that gives AI agents a
persistent identity, a typed lifecycle, and crash-recovery — distributed as a
single binary named **`cs`**, not as a Node module.

## Install the `cs` binary

Prebuilt binaries for Linux and macOS are attached to every
[GitHub Release](https://github.com/noogram/cosmon/releases).

From source (requires a Rust toolchain):

```sh
cargo install --git https://github.com/noogram/cosmon cosmon-cli
```

See <https://docs.noogram.org> for documentation.

---

*Why a placeholder?* Reserving `cosmon` on npm is cheap insurance against
name-squatting once the project is public. Cosmon's audience lives in the
terminal and in Rust, with only incidental overlap into the JS tooling
ecosystem, so this entry is a pure name-hold. If a real npm entry point is ever
wanted (e.g. a thin launcher that downloads the `cs` binary), it replaces this
README in a future minor.
