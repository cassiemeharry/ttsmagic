SHELL := bash
.ONESHELL:
.SHELLFLAGS := -x -eu -o pipefail -c
.MAKEFLAGS += --warn-undefined-variables
.MAKEFLAGS += --no-builtin-rules

ifeq ($(origin .RECIPEPREFIX), undefined)
  $(error This Make does not support .RECIPEPREFIX. Please use GNU Make 4.0 or later)
endif
.RECIPEPREFIX = >

DEPLOY_HOST := ttsmagic.cards
DEPLOY_SSH_COMMAND := "chef-solo -c ~/chef-solo/solo.rb -j ~/chef-solo/dna.json"

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

check-deps:
> cargo udeps --target x86_64-unknown-linux-gnu -p ttsmagic-server --quiet
> cargo udeps --target wasm32-unknown-unknown -p ttsmagic-frontend --quiet

deploy:
> output_filename="ttsmagic-docker-image.tar.gz"
> [[ -h "$$output_filename" ]] && rm "$$output_filename"
> cargo test
> time nix-build nix/docker-image.nix -o "$$output_filename" --show-trace
> time scp "$$output_filename" $(DEPLOY_HOST):"/ttsmagic/$$output_filename"
> rm "$$output_filename"
> time ssh $(DEPLOY_HOST) $(DEPLOY_SSH_COMMAND)
