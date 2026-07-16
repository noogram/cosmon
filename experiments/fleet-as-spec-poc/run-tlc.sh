#!/usr/bin/env bash
# Run TLC on WikiCore.tla with WikiCore.cfg.
#
# Prefers a locally-cached tla2tools.jar (cosmon/docs/specs/tla2tools.jar),
# then $PWD, then downloads to $PWD. Picks up openjdk@17 from brew if
# `java` is not on PATH.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

# --- Resolve Java --------------------------------------------------------
# macOS ships /usr/bin/java as a stub that prompts for install when no JDK
# is linked. We therefore check brew-managed openjdk paths FIRST and fall
# back to `java` on PATH only if those are absent.
if [ -x "/opt/homebrew/opt/openjdk@17/bin/java" ]; then
  JAVA="/opt/homebrew/opt/openjdk@17/bin/java"
elif [ -x "/opt/homebrew/opt/openjdk@21/bin/java" ]; then
  JAVA="/opt/homebrew/opt/openjdk@21/bin/java"
elif [ -x "/opt/homebrew/opt/openjdk/bin/java" ]; then
  JAVA="/opt/homebrew/opt/openjdk/bin/java"
elif command -v java >/dev/null 2>&1; then
  JAVA="java"
else
  echo "ERROR: no java runtime found. Install openjdk (brew install openjdk@17)." >&2
  exit 2
fi

# --- Resolve tla2tools.jar ----------------------------------------------
CANDIDATES=(
  "./tla2tools.jar"
  "$HOME/galaxies/cosmon/docs/specs/tla2tools.jar"
)
JAR=""
for c in "${CANDIDATES[@]}"; do
  if [ -f "$c" ]; then JAR="$c"; break; fi
done

if [ -z "$JAR" ]; then
  echo "tla2tools.jar not found in any cache; downloading..."
  curl -fL -o tla2tools.jar \
    https://github.com/tlaplus/tlaplus/releases/latest/download/tla2tools.jar
  JAR="./tla2tools.jar"
fi

echo "java    : $JAVA"
echo "tla jar : $JAR"
echo "cwd     : $PWD"
echo

# -workers auto uses all cores; -cleanup wipes previous states dir.
exec "$JAVA" -XX:+UseParallelGC -jar "$JAR" \
    -workers auto \
    -cleanup \
    -config WikiCore.cfg \
    WikiCore.tla
