# session-id-patterns.sh — the one definition of "agent-harness session
# identifier", sourced by every surface that refuses one.
#
# WHY THIS IS A LIBRARY AND NOT TWO COPIES
# ----------------------------------------
# Two disjoint surfaces have to refuse the same class: `publish.sh --check`
# reads the tracked tree, and `check-no-session-ids.sh` reads commit messages
# and a pull-request body, which are not in any tree. A class defined twice is
# a class that drifts, and the failure mode of drift here is silent — one
# surface keeps passing while the other has already been widened.
#
# WHAT THE CLASS IS, STATED BY SHAPE AND NOT BY VENDOR
# ----------------------------------------------------
# A harness session identifier names a private conversation that this
# repository's readers cannot and must not open. `Claude-Session:` is the shape
# that motivated the rule; it is an EXAMPLE, never the definition. A rule
# written against one vendor's trailer is defeated the first time a
# codex-piloted session invents its own, so the rules below key on the
# grammar — `<Vendor>-Session:` / `<Vendor>-Thread:` / `Session-Id:`, and any
# `…/session[_/]<opaque-id>` deep link into a vendor console — and name the
# known spellings only in comments.
#
# The detector must contain the shapes it detects (ADR-127 §6), so the one line
# below that carries a literally-matching sample declares itself with the
# per-line waiver marker `publish: allow`, exactly as B and F do. It is one
# line a reviewer reads in the diff, not a pathspec exclusion nobody sees again.
# Known spellings, for the reader — the rules match them by grammar, not by name:
#   Claude-Session: https://claude.ai/code/session_000000000000  # publish: allow — synthetic sample, this file is the detector
#   Codex-Thread:, Session-Id:, chatgpt.com/codex/…, and any vendor console
#   deep link of the form https://<host>/…/session[_/]<opaque-id>.

# A. Vendor-prefixed trailer: `<Anything>-Session:`, `<Anything>-Thread:`,
#    `<Anything>-Conversation:`, with an optional `-Id` / `-Url` tail.
SESSION_ID_TRAILER_RE='^[[:space:]]*[A-Za-z][A-Za-z0-9_-]*[-_](Session|Thread|Conversation)([-_](Id|ID|Url|URL|Link))?:[[:space:]]*[^[:space:]]'

# B. The same trailer with no vendor prefix: `Session-Id:`, `Thread-Url:`.
SESSION_ID_BARE_TRAILER_RE='^[[:space:]]*(Session|Thread|Conversation)[-_](Id|ID|Url|URL|Link):[[:space:]]*[^[:space:]]'

# C. A deep link into a console's session/thread/conversation, whatever the
#    host. The opaque id is what makes it a pointer at one private
#    conversation rather than a link to a product page, so the rule requires
#    one.
SESSION_ID_LINK_RE='https?://[A-Za-z0-9.-]+(/[A-Za-z0-9._-]+)*/(session|sessions|thread|threads|conversation|conversations)[_/-][A-Za-z0-9_-]{4,}'

# D. Vendor consoles whose URL grammar does not spell the word — the path
#    segment itself is the console, and everything under it is one private
#    task or thread.
SESSION_ID_VENDOR_LINK_RE='https?://(chatgpt\.com|chat\.openai\.com)/(codex|c|g)/[A-Za-z0-9_/-]{3,}'

# rule-name<TAB>ERE — iterated by both callers so a new rule is added once.
SESSION_ID_RULES=$'harness-session-trailer\t'"$SESSION_ID_TRAILER_RE"$'\nharness-session-trailer-bare\t'"$SESSION_ID_BARE_TRAILER_RE"$'\nharness-session-deep-link\t'"$SESSION_ID_LINK_RE"$'\nvendor-console-deep-link\t'"$SESSION_ID_VENDOR_LINK_RE"
