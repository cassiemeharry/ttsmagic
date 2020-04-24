SHELL := bash
.ONESHELL:
.SHELLFLAGS := -x -eu -o pipefail -c
.MAKEFLAGS += --warn-undefined-variables
.MAKEFLAGS += --no-builtin-rules

ifeq ($(origin .RECIPEPREFIX), undefined)
  $(error This Make does not support .RECIPEPREFIX. Please use GNU Make 4.0 or later)
endif
.RECIPEPREFIX = >

build:
> nix-build nix/ttsmagic.nix

frontend-impure:
> nix-build nix/frontend.nix
> rm -f ttsmagic-server/static/ttsmagic_frontend*
> cp --no-preserve=ownership,timestamps result/ttsmagic_frontend* ttsmagic-server/static/
> chmod +w ttsmagic-server/static/ttsmagic_frontend*
> rm result

run:
> nix-build nix/ttsmagic.nix
> result/bin/ttsmagic-server

deploy:
> output_filename="ttsmagic-docker-image.tar.gz"
> [[ -h "$$output_filename" ]] && rm "$$output_filename"
> cargo test
> time nix-build nix/docker-image.nix -o "$$output_filename" --show-trace
> time scp "$$output_filename" ttsmagic.cards:"/ttsmagic/$$output_filename"
> rm "$$output_filename"
> time ssh ttsmagic.cards "chef-solo -c ~/chef-solo/solo.rb -j ~/chef-solo/dna.json"
