# Install cosmon

Cosmon ships as **one binary**, `cs`. There is no daemon to run, no service to
register, no account to create: you put a single file on your `PATH` and you are
done.

Pick whichever of the three routes below fits how you already manage tools. The
first two install the **same bytes** — the release pipeline builds the tarballs
once, signs them once, and Homebrew's formula is rendered from those very
artifacts. The third compiles from source, for platforms outside the four
release targets.

Already installed? Skip to [Ten minutes to cosmon](./ten-minutes.md).

## Route 1 — the install script (recommended)

Works on macOS and Linux, on arm64 and x86_64:

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh
```

Then confirm:

```sh
cs --version
cs --help
```

You should see the command groups (lifecycle, fleet, execution, …). If the shell
cannot find `cs`, the installer printed the `export PATH=…` line you need — add
it to your shell profile and re-open the terminal.

### What that one line actually does

Piping a script from the internet into your shell deserves an explanation, so
here is the whole of it. The installer:

1. **Detects your platform** from `uname -s` and `uname -m`, and maps it to one
   of the four targets cosmon builds: macOS on arm64 or x86_64, Linux on x86_64
   or arm64. Anything else is refused with a clear message rather than guessed
   at. To see what it resolves for your machine without installing anything:
   `curl -fsSL https://noogram.org/cosmon/install.sh | sh -s -- --print-target`.
2. **Downloads the release `SHA256SUMS`** from the GitHub Releases of
   [`noogram/cosmon`](https://github.com/noogram/cosmon/releases). That file is
   the source of truth for both the exact tarball name and its digest, which is
   how the installer can ask for `latest` without knowing the version string up
   front.
3. **Downloads the tarball** for your target over HTTPS (`--proto '=https'
   --tlsv1.2`), using `curl` or `wget`, whichever you have.
4. **Verifies the sha256** of what it downloaded against `SHA256SUMS`. This leg
   is **fail-closed**: a mismatch, a missing digest, or no `sha256sum`/`shasum`
   on the box all abort the install rather than proceeding. Nothing is written
   to your `PATH` before the digest matches.
5. **Unpacks and installs** `cs` into `~/.local/bin`, falling back to
   `/usr/local/bin` if that directory is not writable. The tarball also carries
   `cosmon-remote` — the connector for driving a remote cosmon service — and the
   installer places it in the same directory, so one command gives you both
   laptop tools.

It carries no secret and needs no privilege beyond writing to that one
directory.

### Choosing a version

The default is the **latest** release. To pin a specific one, either flag or
environment works:

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh -s -- --version v0.1.0
# or
curl -fsSL https://noogram.org/cosmon/install.sh | COSMON_VERSION=v0.1.0 sh
```

`v0.1.0` is the tag format the installer expects, shown here as an example —
pinning any specific version requires that tag to actually exist as a published
release. `--dir <path>` (or `COSMON_INSTALL_DIR`) changes where `cs` lands.

## Route 2 — Homebrew

Since **v0.2.0** the tap [`noogram/homebrew-tap`](https://github.com/noogram/homebrew-tap)
is live, on macOS and on Linuxbrew, arm64 and x86_64 alike:

```sh
brew install noogram/tap/cosmon
```

This is not a separate build. The release pipeline renders the formula from the
*same* tagged, signed release tarballs the install script downloads, and `brew`
verifies the *same* sha256 digests. Identical bytes, identical provenance.

## Route 3 — build from source

If you would rather compile it yourself — or you are on a platform outside the
four release targets — build from the cosmon repository:

```sh
git clone https://github.com/noogram/cosmon && cd cosmon
cargo install --path crates/cosmon-cli --locked
```

Re-run `cs --help` afterwards to confirm it landed on your `PATH`. Note that a
source build is *your* build: it is not covered by the release signature, so the
provenance check below does not apply to it.

## Verify where the binary came from

The sha256 check proves the bytes match the digest **the release published**. It
does not, on its own, prove *who produced* that release. That proof is a `cosign`
signature check you run once, deliberately, and it is worth doing:

→ [Verify the binary's provenance](../how-to/verify-the-binary.md)

## A note on package registries

The product **is** the `cs` binary published on GitHub Releases, versioned by the
git tag it was built from. If cosmon ever appears on crates.io, npm, or PyPI,
**those entries are name-holds, not the shipped binary** — they exist to hold the
name and point back here. Do not expect `cargo install cosmon` /
`npm install cosmon` / `pip install cosmon` to give you the released binary.

## Next

- [Ten minutes to cosmon](./ten-minutes.md) — run one piece of work end to end.
- [Set up cosmon (prerequisites)](../tutorials/setup.md) — the other tools a
  worker needs (git, tmux, a model backend) before the tutorials.
