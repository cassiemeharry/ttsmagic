#!/usr/bin/env bash

set -x
set -euo pipefail

output_filename="ttsmagic-docker-image.tar.gz"

[[ -h "$output_filename" ]] && rm "$output_filename"
cargo test
nix-build nix/docker-image.nix -o "$output_filename"
scp "$output_filename" ttsmagic.cards:"/ttsmagic/$output_filename"
rm "$output_filename"
# ssh ttsmagic.cards "docker load -i /ttsmagic/$output_filename"
ssh ttsmagic.cards "chef-solo -c ~/chef-solo/solo.rb -j ~/chef-solo/dna.json"
