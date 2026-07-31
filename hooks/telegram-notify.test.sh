#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Exercise hooks/telegram-notify.sh's message construction — without sending
# anything.
#
# # Why this file exists
#
# `escape_html` was wrong for two of the three characters it exists to escape,
# and shipped that way. On bash 5.2 and later, an unescaped `&` in the
# REPLACEMENT half of `${var//pat/repl}` stands for the matched text, as in
# `sed`. So `${text//>/&gt;}` replaced `>` with `>gt;`, and every notification
# carrying an angle bracket arrived mangled.
#
# It survived because nothing exercised it, and because the one case a reader
# would spot-check by eye — `&` — is accidentally correct: the matched text
# there IS `&`, so `&amp;` comes out right while its two neighbours do not. A
# function whose easiest test case passes by coincidence needs the other two
# written down.
#
# Hermetic: sources the two pure functions out of the hook and calls them
# directly. No network, no token, no Telegram.

set -uo pipefail
cd "$(dirname "$0")/.."
HOOK="$PWD/hooks/telegram-notify.sh"

pass=0; fail=0
ok() { printf '  \033[32m✓\033[0m %s\n' "$1"; pass=$((pass+1)); }
ko() { printf '  \033[31m✗\033[0m %s\n      expected: %s\n      actual:   %s\n' \
       "$1" "$2" "$3"; fail=$((fail+1)); }

eq() { # eq <name> <expected> <actual>
  if [[ "$2" == "$3" ]]; then ok "$1"; else ko "$1" "$2" "$3"; fi
}

# Lift the escaper out of the hook rather than re-implementing it: a copy here
# would pass while the shipped one stayed broken, which is the failure mode
# this file is about.
eval "$(sed -n '/^escape_html()/,/^}/p' "$HOOK")"

echo "── telegram-notify.sh ────────────────────────────────────────────────"

eq "a greater-than becomes an entity, not a matched-text splice" \
   'a&gt;b' "$(escape_html 'a>b')"

eq "a less-than becomes an entity, not a matched-text splice" \
   '&lt;tag&gt;' "$(escape_html '<tag>')"

eq "an ampersand is escaped once, not twice" \
   'a&amp;b' "$(escape_html 'a&b')"

# The ordering rule: `&` is escaped first, so the ampersands the later rules
# introduce must not be escaped again. If the order were reversed, this would
# come back as `a&amp;lt;b`.
eq "an already-entity-looking input is not double-escaped past one pass" \
   'a&amp;lt;b' "$(escape_html 'a&lt;b')"

eq "text with none of the three is returned unchanged" \
   'plain phase report 1 to 2' "$(escape_html 'plain phase report 1 to 2')"

# The realistic payload: a phase transition arrow, which is what surfaced the
# bug in the field on 2026-07-31.
eq "a phase arrow survives the round trip" \
   '[P1-&gt;P2] gates green' "$(escape_html '[P1->P2] gates green')"

echo "──────────────────────────────────────────────────────────────────────"
echo "telegram-notify.test: $pass passed, $fail FAILED."
[[ $fail -eq 0 ]]
