// Noogram apex install Worker — serves raw installer shell at
// `noogram.org/<tool>/install.sh`.
//
// This is the doorbell the C2 DNS config (infra/cloudflare/) resolves the
// address for. It owns exactly one surface: the per-tool install endpoints on
// the `noogram.org` apex. Everything else on the apex is out of its scope
// (docs live at docs.noogram.org; the project landing is a later concern).
//
// ── The one gate (delib-20260711-8d00, Q4 = Q5) ──────────────────────────────
// A `/<tool>/install.sh` endpoint exists *iff* the tool ships a public
// per-platform binary. The path table below is the mechanical projection of
// that rule: `cosmon` is wired (it has a release matrix); every other path is
// an honest 404 until some tool earns it. The private `neurion` product has no
// public binary, so it can never acquire a path here — the guard is structural,
// not a matter of anyone remembering to keep it out.
//
// ── STAGING (this is why nothing is live) ────────────────────────────────────
// The `PUBLISHED` var gates the real script. It defaults to "false", so a
// deploy of this Worker today serves an honest 503 "coming soon" for
// /cosmon/install.sh. The operator flips it to "true" (one `wrangler deploy`
// after the var flip) only once `noogram/cosmon` is public AND the first
// release is tagged. Until then, `curl -fsSL … | sh` is a safe no-op (curl -f
// exits non-zero on the 503 and pipes nothing), and a bare `curl` prints the
// coming-soon note. See ./RUNBOOK.md for the activation steps.
//
// The served script is the single canonical infra/install/install.sh, imported
// as text at build time — there is no second copy to drift.

import COSMON_INSTALL_SH from "../../install.sh";

interface Env {
  // "true" ⇒ serve the real installer; anything else ⇒ 503 coming-soon.
  // Set as a Worker var (wrangler [vars] or dashboard). Default staged.
  PUBLISHED?: string;
}

// Tools with a live public install surface. cosmon is the only one today. Add a
// row ONLY when the tool has a public repo with a resolving per-platform
// release — that is the whole gate.
const TOOLS: Record<string, string> = {
  cosmon: COSMON_INSTALL_SH,
};

const TEXT = "text/plain; charset=utf-8";

// A `?version=` pin, sanitized. We accept a conservative tag charset and inject
// it as a leading `COSMON_VERSION=…` line so the canonical install.sh stays
// placeholder-free (it just reads the env var). Anything unsafe is dropped.
function versionPin(url: URL): string | null {
  const v = url.searchParams.get("version");
  if (!v) return null;
  return /^v?[0-9A-Za-z][0-9A-Za-z.\-_]{0,63}$/.test(v) ? v : null;
}

function comingSoon(tool: string): Response {
  // 503 body is itself shell-safe: piped to `sh` it prints and exits 1
  // (non-destructive). `curl -f` never reaches this — it fails on the 503.
  const body =
    `#!/bin/sh\n` +
    `echo "Noogram: ${tool} public install is not live yet — coming soon." >&2\n` +
    `echo "Track it: https://github.com/noogram/${tool}" >&2\n` +
    `exit 1\n`;
  return new Response(body, {
    status: 503,
    headers: {
      "content-type": TEXT,
      "retry-after": "86400",
      "cache-control": "no-store",
      "x-robots-tag": "noindex",
    },
  });
}

function notFound(msg: string): Response {
  return new Response(`${msg}\n`, {
    status: 404,
    headers: { "content-type": TEXT, "x-robots-tag": "noindex" },
  });
}

function serveScript(script: string, pin: string | null): Response {
  // Prepend the pin (if any) before the shebang. When piped to `sh` the
  // shebang is a comment, so a leading assignment runs first and install.sh
  // picks it up via `${COSMON_VERSION:-}`.
  const body = pin ? `COSMON_VERSION='${pin}'\n${script}` : script;
  return new Response(body, {
    status: 200,
    headers: {
      "content-type": TEXT,
      // Short cache: the script rarely changes, but we want a flip (staging →
      // live, or a script fix) to propagate within minutes.
      "cache-control": "public, max-age=300",
      "x-content-type-options": "nosniff",
    },
  });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const path = url.pathname;
    const published = env.PUBLISHED === "true";

    // `/<tool>/install.sh`
    const m = path.match(/^\/([a-z][a-z0-9-]{0,31})\/install\.sh$/);
    if (m) {
      const tool = m[1];
      const script = TOOLS[tool];
      if (!script) {
        // Reserved scheme, but this tool has no public binary → honest 404.
        return notFound(`No public installer for "${tool}" (yet). See https://noogram.org`);
      }
      if (!published) return comingSoon(tool);
      return serveScript(script, versionPin(url));
    }

    // `/install.sh` — reserved for a future distro meta-installer. Not shipped.
    if (path === "/install.sh") {
      return comingSoon("noogram");
    }

    // Everything else on the apex — a minimal, honest placeholder. The project
    // landing page is a separate concern; docs live at docs.noogram.org.
    if (path === "/" || path === "") {
      const body =
        "Noogram — a distribution for agent fleets, built on the cosmon kernel.\n\n" +
        "Docs:    https://docs.noogram.org\n" +
        "Source:  https://github.com/noogram/cosmon\n\n" +
        (published
          ? "Install cosmon:  curl -fsSL https://noogram.org/cosmon/install.sh | sh\n"
          : "Public install is coming soon.\n");
      return new Response(body, {
        status: 200,
        headers: { "content-type": TEXT },
      });
    }

    return notFound("Not found. See https://noogram.org");
  },
};
