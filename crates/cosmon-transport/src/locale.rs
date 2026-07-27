// SPDX-License-Identifier: AGPL-3.0-only

//! UTF-8 locale resolution for the tmux-backed worker surface.
//!
//! # The defect this module exists for
//!
//! cosmon chooses to drive an interactive worker as a **TUI inside a tmux
//! pane**. That interface is made of box-drawing characters, bullets and
//! arrows; a reader whose terminal is not declared UTF-8 does not see them.
//! In a container with no locale configured — `LANG` unset, `LC_ALL` unset,
//! `locale` reporting `LC_CTYPE="POSIX"`, which is the ordinary default of a
//! slim Debian base and *not* a misconfiguration — every non-ASCII glyph of
//! the worker's screen is replaced by `_`, and text is corrupted with it
//! (`Cramér` renders as `Cram_r`).
//!
//! Measured 2026-07-27 in `debian:bookworm-slim` + tmux 3.3a, both halves of
//! the differential:
//!
//! ```text
//! tmux capture-pane           → "Cramér •"   (pane buffer INTACT)
//! attach, LC_CTYPE=POSIX      → "Cram_r _"
//! attach, LC_ALL=C.UTF-8      → "Cramér •"
//! ```
//!
//! The application writes correct UTF-8 and tmux stores it correctly. The
//! substitution happens when tmux **draws to a client** whose locale does not
//! declare UTF-8: tmux then believes the terminal cannot represent those
//! characters and prints `_` instead.
//!
//! # Which side owns the fix — measured, not assumed
//!
//! The rendering decision is taken **per attaching client**, not by the
//! server. Also measured, in the same image: a tmux server started with
//! `LC_ALL=C.UTF-8` still renders `Cram_r _` to a client attaching under
//! `POSIX`. So exporting a locale on cosmon's side of the fence does *not*
//! fix what a human later sees — nothing cosmon does to the server can.
//! That is why this module has two distinct outputs:
//!
//! 1. [`command_prefix`] (applied by [`with_utf8_floor_from_env`] at the
//!    single tmux spawn choke point,
//!    [`TmuxBackend::spawn_worker`](crate::TmuxBackend::spawn_worker), so all
//!    four tmux-backed adapters are covered once). This is the part cosmon
//!    controls: the **pane process's own** environment. It does not change
//!    how a later attach renders; it makes the worker and anything it
//!    spawns (including a `tmux attach` run from *inside* the pane, which
//!    inherits this env and is a client) locale-correct.
//! 2. [`attach_command`] — the attach line `cs tackle` prints. This is the
//!    part cosmon does **not** control: a human attaching from outside, in a
//!    process cosmon never spawns. cosmon cannot set that process's
//!    environment, so it makes the requirement *discoverable* instead of
//!    silent — the printed line carries the locale, and a user who copies
//!    the line cosmon gave them gets a legible screen.
//!
//! Nothing here can help a user who attaches with a hand-written command in a
//! POSIX-locale shell. That case is documented, not fixed.
//!
//! # Deliberately non-destructive
//!
//! If `LC_ALL`, `LC_CTYPE` or `LANG` already declares UTF-8 (POSIX
//! precedence), every function here is a no-op: an operator who chose a
//! locale keeps it, and the emitted command stays byte-identical to the
//! pre-fix shape. Only the *nothing-declared* case is repaired.
//!
//! # Why no gate caught this
//!
//! Same family as the container doors of issue #20: the defect exists only on
//! a screen a human is looking at. `cargo test` never attaches a terminal;
//! `capture-pane` — what every automated probe in this repo reads — returns
//! the *intact* bytes. Build, test, clippy, fmt and doc are all green while
//! the interface is unreadable. The only detector is a human eye on an
//! attached pane.

use std::process::Command;

/// The locale cosmon supplies when the environment declares none.
///
/// `C.UTF-8` is present in glibc images without the `locales` package —
/// verified by running it, not assumed: in `debian:bookworm-slim`,
/// `locale -a` lists the name as `C.utf8`, yet `LC_ALL=C.UTF-8 locale`
/// resolves to `LC_CTYPE="C.UTF-8"` and exits 0, because glibc normalises
/// the codeset spelling. It is used only as the last resort, after
/// [`available_locales`] has been given the chance to name a locale the
/// host actually lists.
pub const FALLBACK_UTF8_LOCALE: &str = "C.UTF-8";

/// Locale names preferred, in order, when picking from what the host lists.
const PREFERRED: &[&str] = &["c.utf8", "en_us.utf8", "c.utf-8", "en_us.utf-8"];

/// Return `true` when `env_lookup` already declares a UTF-8 locale.
///
/// Consults `LC_ALL`, `LC_CTYPE` and `LANG` in POSIX precedence order and
/// stops at the first non-empty one — that variable, and only that one,
/// decides `LC_CTYPE` for a child process. A non-empty `LC_ALL=POSIX`
/// therefore reports `false` even if `LANG` names a UTF-8 locale, which is
/// exactly how the C library resolves it.
pub fn declares_utf8<F>(env_lookup: &F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(value) = env_lookup(key).filter(|v| !v.is_empty()) {
            return is_utf8_name(&value);
        }
    }
    false
}

/// Return `true` when a locale name declares the UTF-8 codeset.
///
/// Matches both spellings the two families use (`C.UTF-8`, `C.utf8`), case
/// insensitively, so `en_US.UTF-8` and `en_US.utf8` are the same answer.
fn is_utf8_name(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered.contains("utf-8") || lowered.contains("utf8")
}

/// List the locale names the host declares, via `locale -a`.
///
/// Returns an empty vector when the binary is absent (a slim image may ship
/// no `locale` at all), when it exits non-zero, or when its output is empty.
/// An empty answer is not an error: [`resolve`] falls back to
/// [`FALLBACK_UTF8_LOCALE`], which glibc accepts without the `locales`
/// package.
#[must_use]
pub fn available_locales() -> Vec<String> {
    let Ok(output) = Command::new("locale").arg("-a").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Resolve the UTF-8 locale to supply, or `None` when one is already declared.
///
/// `available` is called **only** when nothing is declared, so the common
/// path (a developer workstation with `LANG=en_US.UTF-8`) never shells out.
/// Selection order: the preferred names (`C.utf8`, then `en_US.utf8`, in
/// either codeset spelling) as listed by the host, then any
/// other listed UTF-8 locale, then [`FALLBACK_UTF8_LOCALE`]. The name is
/// returned **as the host spells it** whenever it came from the list, so it
/// is a name `setlocale` is known to accept on that host.
pub fn resolve<F, P>(env_lookup: &F, available: P) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
    P: FnOnce() -> Vec<String>,
{
    if declares_utf8(env_lookup) {
        return None;
    }
    let listed = available();
    for preferred in PREFERRED {
        if let Some(found) = listed
            .iter()
            .find(|name| name.to_ascii_lowercase() == *preferred)
        {
            return Some(found.clone());
        }
    }
    if let Some(any_utf8) = listed.iter().find(|name| is_utf8_name(name)) {
        return Some(any_utf8.clone());
    }
    Some(FALLBACK_UTF8_LOCALE.to_owned())
}

/// Build the `LC_ALL=<locale> ` prefix for a spawned command string.
///
/// Empty when the environment already declares UTF-8 — the emitted command
/// is then byte-identical to the pre-fix shape. Otherwise a trailing-space
/// fragment suitable for concatenation ahead of the next `VAR=value` token,
/// matching the assembly convention of `cosmon_cli::tackle_env`'s
/// `build_claude_command`.
///
/// This covers the pane process only; see the module doc for what it does
/// *not* cover.
pub fn command_prefix<F, P>(env_lookup: &F, available: P) -> String
where
    F: Fn(&str) -> Option<String>,
    P: FnOnce() -> Vec<String>,
{
    match resolve(env_lookup, available) {
        Some(locale) => format!("LC_ALL={locale} "),
        None => String::new(),
    }
}

/// Prefix a to-be-spawned command with the UTF-8 floor, reading the live
/// process environment.
///
/// Returns `cmd` unchanged when a UTF-8 locale is already declared — the
/// workstation path stays byte-identical. Otherwise returns
/// `LC_ALL=<locale> <cmd>`; tmux runs a session command through the shell,
/// which applies the assignment to that command exactly like the
/// `CLAUDE_CONFIG_DIR=… claude …` prefixes the adapters already emit.
#[must_use]
pub fn with_utf8_floor_from_env(cmd: &str) -> String {
    with_utf8_floor(cmd, &|key: &str| std::env::var(key).ok(), available_locales)
}

/// [`with_utf8_floor_from_env`] with the environment and the locale probe
/// injected, so the decision is testable without mutating the process env.
pub fn with_utf8_floor<F, P>(cmd: &str, env_lookup: &F, available: P) -> String
where
    F: Fn(&str) -> Option<String>,
    P: FnOnce() -> Vec<String>,
{
    let prefix = command_prefix(env_lookup, available);
    format!("{prefix}{cmd}")
}

/// Build the attach command line `cs tackle` prints for a tmux-backed worker.
///
/// When the spawn environment declares UTF-8 the line is the bare
/// `tmux -L <socket> attach -t <session>` it has always been. When it does
/// not, the line carries the locale (`LC_ALL=… tmux -L …`) so a user who
/// copies what cosmon printed gets a legible screen instead of a field of
/// underscores. The prefix is a plain shell assignment, so it survives the
/// usual wrappers (`sh -c`, `docker exec … sh -c`).
pub fn attach_command<F, P>(socket: &str, session: &str, env_lookup: &F, available: P) -> String
where
    F: Fn(&str) -> Option<String>,
    P: FnOnce() -> Vec<String>,
{
    let prefix = command_prefix(env_lookup, available);
    format!("{prefix}tmux -L {socket} attach -t {session}")
}

/// Build the attach line from the live process environment.
///
/// Thin convenience over [`attach_command`] for the printing call sites in
/// `cs tackle`, which have no injected environment to hand.
#[must_use]
pub fn attach_command_from_env(socket: &str, session: &str) -> String {
    attach_command(
        socket,
        session,
        &|key: &str| std::env::var(key).ok(),
        available_locales,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `env_lookup` over a fixed table, absent for anything else.
    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + use<'a> {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn debian_slim() -> Vec<String> {
        // Exactly what `locale -a` prints in debian:bookworm-slim.
        ["C", "C.utf8", "POSIX"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    #[test]
    fn declared_utf8_locale_is_left_untouched() {
        for declared in [
            [("LANG", "en_US.UTF-8")],
            [("LC_CTYPE", "fr_FR.utf8")],
            [("LC_ALL", "C.UTF-8")],
        ] {
            let env = env_of(&declared);
            assert!(declares_utf8(&env), "{declared:?} declares UTF-8");
            assert_eq!(
                resolve(&env, || panic!("must not probe when UTF-8 is declared")),
                None,
                "{declared:?} must be left alone"
            );
            assert_eq!(
                command_prefix(&env, Vec::new),
                "",
                "{declared:?} must emit no prefix"
            );
        }
    }

    #[test]
    fn nothing_declared_yields_a_utf8_default_from_the_host_list() {
        let env = env_of(&[]);
        assert!(!declares_utf8(&env));
        assert_eq!(resolve(&env, debian_slim), Some("C.utf8".to_owned()));
        assert_eq!(command_prefix(&env, debian_slim), "LC_ALL=C.utf8 ");
    }

    #[test]
    fn posix_locale_is_repaired_not_honoured() {
        // The container case: a locale IS declared, but it is not UTF-8.
        let env = env_of(&[("LC_ALL", "POSIX"), ("LANG", "C")]);
        assert!(!declares_utf8(&env));
        assert_eq!(resolve(&env, debian_slim), Some("C.utf8".to_owned()));
    }

    #[test]
    fn empty_values_fall_through_to_the_next_variable() {
        // An exported-but-empty LC_ALL does not decide LC_CTYPE.
        let env = env_of(&[("LC_ALL", ""), ("LANG", "en_US.UTF-8")]);
        assert!(declares_utf8(&env));
    }

    #[test]
    fn lc_all_wins_over_a_utf8_lang() {
        // POSIX precedence: a non-empty LC_ALL decides, even against LANG.
        let env = env_of(&[("LC_ALL", "POSIX"), ("LANG", "en_US.UTF-8")]);
        assert!(!declares_utf8(&env));
    }

    #[test]
    fn host_without_a_utf8_locale_falls_back_to_c_utf8() {
        let env = env_of(&[]);
        let bare = || vec!["C".to_owned(), "POSIX".to_owned()];
        assert_eq!(resolve(&env, bare), Some(FALLBACK_UTF8_LOCALE.to_owned()));
        // And when `locale` is absent entirely (empty list).
        assert_eq!(
            resolve(&env, Vec::new),
            Some(FALLBACK_UTF8_LOCALE.to_owned())
        );
    }

    #[test]
    fn attach_line_carries_the_locale_only_when_one_is_missing() {
        let declared = env_of(&[("LANG", "en_US.UTF-8")]);
        assert_eq!(
            attach_command("cosmon", "cs-worker", &declared, Vec::new),
            "tmux -L cosmon attach -t cs-worker"
        );

        let bare = env_of(&[]);
        assert_eq!(
            attach_command("cosmon", "cs-worker", &bare, debian_slim),
            "LC_ALL=C.utf8 tmux -L cosmon attach -t cs-worker"
        );
    }

    #[test]
    fn the_floor_leaves_a_declared_host_command_byte_identical() {
        let cmd = "CLAUDE_CONFIG_DIR=/home/w/.claude claude --permission-mode bypassPermissions";
        let declared = env_of(&[("LC_ALL", "C.UTF-8")]);
        assert_eq!(with_utf8_floor(cmd, &declared, Vec::new), cmd);

        let bare = env_of(&[]);
        assert_eq!(
            with_utf8_floor(cmd, &bare, debian_slim),
            format!("LC_ALL=C.utf8 {cmd}"),
            "the floor goes ahead of the adapter's own VAR=value prefix"
        );
    }

    #[test]
    fn preference_order_picks_c_utf8_over_a_regional_locale() {
        let env = env_of(&[]);
        let many = || {
            ["POSIX", "zh_CN.utf8", "en_US.utf8", "C.utf8"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        };
        assert_eq!(resolve(&env, many), Some("C.utf8".to_owned()));
    }

    #[test]
    fn an_unpreferred_utf8_locale_is_better_than_the_hardcoded_fallback() {
        let env = env_of(&[]);
        let only_regional = || vec!["POSIX".to_owned(), "fr_FR.UTF-8".to_owned()];
        assert_eq!(resolve(&env, only_regional), Some("fr_FR.UTF-8".to_owned()));
    }
}
