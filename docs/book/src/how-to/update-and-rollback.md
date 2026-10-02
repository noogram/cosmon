# Update and roll back cosmon

> **One line.** Updating and rolling back are the same gesture: run the
> installer (or `brew`) again, naming the release you want, then check which
> binaries your shell now finds. Binaries and project state are separate things;
> this page covers the binaries and says where the state boundary lies.

It applies to the two client programs installed by [Install cosmon](../getting-started/install.md):
`cs` and `cosmon-remote`.

## 1. Check what is installed

```sh
command -v cs
command -v cosmon-remote
cs --version
cosmon-remote --version
```

Note both paths, both versions, and the route that put them there (script,
Homebrew, or a source build). Two installations can coexist, and a shell may
cache an old path: if a version is not the one you just installed, run
`hash -r` (or open a new terminal) and compare `command -v` with the install
directory. A missing `cosmon-remote` is a finding, not a sign that the
installation is current.

## 2. Prepare

- Read the release notes of the target release and the
  [versioning policy](../explanation/versioning.md). Before 1.0, a minor release
  may change CLI output, JSON, and state formats.
- Let active work finish before a coordinated change.
- Back up the galaxy's state and configuration. A commit or a worktree copy can
  miss ignored files, so copy the directory that actually holds them (normally
  `.cosmon/` at the galaxy root) while nothing is writing to it.
- `cosmon-remote` profiles and credentials live in the platform configuration
  directory, or in the keyring, a file, or the environment, depending on the
  backend you chose. An upgrade does not touch them; if you back them up, keep
  them as protected as they were.

## 3. Update

Use the route you installed with.

**Install script.** Run it again with the directory you noted in step 1:

```sh
curl -fsSL https://noogram.org/cosmon/install.sh | sh -s -- --dir "$HOME/.local/bin"
```

With no version it takes the latest release. An exported `COSMON_VERSION` or
`COSMON_INSTALL_DIR` is honored, so an old pin left in your environment keeps
you on the old release: `env | grep COSMON_` shows it. To verify the installer
before running it, use the download-and-verify route in
[Install cosmon](../getting-started/install.md#the-same-route-verifying-the-installer-first);
the versioned installer file does not pin its payload by itself, so still pass
`--version`.

**Homebrew.**

```sh
brew update
brew upgrade noogram/tap/cosmon
```

A formula pinned with `brew pin` is skipped until you `brew unpin` it.

Then repeat step 1. Both programs should report the new version.

## 4. Roll back to a published tag

Choose a tag that exists among the
[releases](https://github.com/noogram/cosmon/releases) for your platform, and
install it over the current one, in the same directory:

```sh
target_tag=v0.6.0        # replace with the tag you want
install_dir="$HOME/.local/bin"

curl -fsSL https://noogram.org/cosmon/install.sh | sh -s -- --version "$target_tag" --dir "$install_dir"
# or
curl -fsSL https://noogram.org/cosmon/install.sh | COSMON_VERSION="$target_tag" sh -s -- --dir "$install_dir"
```

The installer checks the tarball's sha256 before it writes anything, so a
corrupt download leaves the current binaries in place. It then replaces `cs` and
`cosmon-remote` one after the other: if it fails part-way, run step 1 and check
both. Check `"$install_dir/cs" --version` and `"$install_dir/cosmon-remote"
--version` first, then the ones your shell finds.

If the older archive has no `cosmon-remote`, the installer warns and installs
`cs` only. Into an existing directory, the connector already there stays, at its
current version; into a new directory, there is none. Neither is a paired
rollback. Pick a release that ships both programs, or knowingly use the older
`cs` alone.

**Homebrew.** There is no package downgrade command here, and you should not
write into the package prefix. Install the tagged pair into a directory of your
own and put it first on `PATH`:

```sh
mkdir -p "$HOME/.cosmon-pinned/$target_tag"
curl -fsSL https://noogram.org/cosmon/install.sh | sh -s -- --version "$target_tag" --dir "$HOME/.cosmon-pinned/$target_tag"
export PATH="$HOME/.cosmon-pinned/$target_tag:$PATH"
hash -r; cs --version; cosmon-remote --version
```

To return to the managed installation, remove that line from `PATH`, run
`hash -r`, and check the versions again.

## 5. State and running processes

Replacing the binaries does not rewrite galaxy state, profiles, or credentials.
Commands run by the newer version may, though, write in a newer format, and no
release pair is certified as readable by the older one. Follow the migration
notes of the release you move to. If an older `cs` cannot use the current state,
stop and restore from the backup of step 2 rather than deleting state, resetting
the repository, or overwriting work done since the upgrade.

Two commands have names that suggest more than they do:

- `cs init --upgrade` refreshes project material (missing files, ignore rules,
  the managed instruction block). It does not install or downgrade binaries.
- `cs migrate rollback` reverses a state residence move. It is not a release
  rollback.

A process that is already running keeps the binary it started from. Start a
fresh `cs` session, or restart your own long-running clients, after replacing
the files. Updating `cosmon-remote` on your machine does not deploy or restart
the remote service; the host side is covered in
[Run cosmon as a remote service](./deploy-remote-service.md).
