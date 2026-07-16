#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1784073600}"

build_one() {
  local source="$1"
  local variant="$2"
  local width="$3"
  local stem="${source%.tex}${variant}"
  local mode=""
  if [[ "$variant" == "-light" ]]; then
    mode='\def\lightmode{1}'
  fi

  rm -f "${stem}.aux" "${stem}.log" "${stem}.pdf"
  lualatex --interaction=nonstopmode --halt-on-error \
    --jobname="$stem" "${mode}\input{${source}}" >/dev/null

  if grep -Eq '(^!|Overfull \\hbox|Overfull \\vbox)' "${stem}.log"; then
    grep -E '(^!|Overfull \\hbox|Overfull \\vbox)' "${stem}.log" >&2
    exit 1
  fi

  pdf2svg "${stem}.pdf" "${stem}.svg"
  inkscape "${stem}.svg" --export-type=png --export-filename="${stem}.png" \
    --export-width="$width" >/dev/null
  rm -f "${stem}.aux" "${stem}.log" "${stem}.pdf"
}

build_one intro-synthetic.tex "" 1600
build_one intro-synthetic.tex -light 1600
build_one how-cosmon-runs.tex "" 1600
build_one how-cosmon-runs.tex -light 1600
build_one deploy-remote-service.tex "" 1600
build_one deploy-remote-service.tex -light 1600

cp intro-synthetic.svg intro-synthetic-light.svg \
  how-cosmon-runs.svg how-cosmon-runs-light.svg \
  deploy-remote-service.svg deploy-remote-service-light.svg ..

identify intro-synthetic.png intro-synthetic-light.png \
  how-cosmon-runs.png how-cosmon-runs-light.png \
  deploy-remote-service.png deploy-remote-service-light.png

# The remote-service page ships the transparent SVG pair. Keep the PNGs only
# long enough for `identify` to validate their rasterisation; unlike the older
# compatibility assets above, they are not public inputs and must not linger.
rm -f deploy-remote-service.png deploy-remote-service-light.png
