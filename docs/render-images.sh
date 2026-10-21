#!/bin/sh
# Regenerates the README screenshots with dmsh's headless renderer.
# Run from the repository root: sh docs/render-images.sh
set -eu
dmsh=${DMSH:-target/release/dmsh}
bg='#2c2c30'
out=docs/images
f=docs/fixtures

"$dmsh" render --activities $f/islands.json --battery 76 --at 1s --background "$bg" --crop 560,1085 -o $out/islands.png
"$dmsh" render --activities $f/flow.json --battery 76 --at 3s --filmstrip 300ms --background "$bg" --crop 560,1085 -o $out/flow.png
"$dmsh" render --activities $f/question.json --at 1s --hover center --background "$bg" --crop 600,445 -o $out/question.png
"$dmsh" render --activities $f/media.json --at 1s --hover center --background "$bg" --crop 600,445 -o $out/media.png
"$dmsh" render --activities $f/islands.json --battery 76+ --wifi on --volume 45 --mic 60 --brightness 70 --at 1s --hover right --background "$bg" --crop 1285,360 -o $out/right-expanded.png
"$dmsh" render --at 1s --hover center --background "$bg" --crop 690,265 -o $out/clock.png
"$dmsh" render --battery 76 --at 1s --hover right --session reboot:0.6 --background "$bg" --crop 1285,360 -o $out/session.png
"$dmsh" render --battery 76 --volume 45 --brightness 70 --slider volume --at 1s --hover right --background "$bg" --crop 1285,360 -o $out/volume.png
