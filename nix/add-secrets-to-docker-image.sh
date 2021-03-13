#!/usr/bin/env bash

set -euo pipefail
set -x

BASE_IMAGE_FILE="$1"
SECRETS_FILE="$2"

WORK_DIR="$(mktemp --directory)"
trap '{ rm -rf -- "$WORK_DIR"; }' EXIT

cd "$WORK_DIR"

# Extract the original image to start from
tar xzf "$BASE_IMAGE_FILE"

# Create the new layer
mkdir -p secrets-layer/etc/ttsmagic/
cp "$SECRETS_FILE" secrets-layer/etc/ttsmagic/secrets.toml
cd secrets-layer/
tar cf layer.tar --absolute-names ./etc/
rm -r etc/
tar tf layer.tar
LAYER_HASH="$(openssl sha256 layer.tar | awk '{print $2}')"
cd ..
mv secrets-layer/ "$LAYER_HASH/"

# Add the layer to the manifest files
OLD_CONFIG_FILENAME="$(jq -r < manifest.json '.[0].Config')"
NOW="$(date --iso-8601=seconds)"
jq < "$OLD_CONFIG_FILENAME" > "$OLD_CONFIG_FILENAME.new" "
setpath([\"created\"]; \"$NOW\")
| setpath([\"rootfs\", \"diff_ids\"]; getpath([\"rootfs\", \"diff_ids\"]) + [\"sha256:$LAYER_HASH\"])
| setpath([\"history\"]; getpath([\"history\"]) + [{\"created\": \"$NOW\", \"comment\": \"Added /etc/ttsmagic/secrets.toml\"}])
"
mv "$OLD_CONFIG_FILENAME.new" "$OLD_CONFIG_FILENAME"
NEW_CONFIG_HASH="$(openssl sha256 "$OLD_CONFIG_FILENAME" | awk '{print $2}')"
NEW_CONFIG_FILENAME="$NEW_CONFIG_HASH.json"
mv "$OLD_CONFIG_FILENAME" "$NEW_CONFIG_FILENAME"
jq < "$NEW_CONFIG_FILENAME" .

jq < manifest.json > manifest.json.new "
setpath([0, \"Layers\"]; getpath([0, \"Layers\"]) + [\"$LAYER_HASH/layer.tar\"])
| setpath([0, \"Config\"]; \"$NEW_CONFIG_FILENAME\")
"
mv manifest.json.new manifest.json
openssl sha256 manifest.json | awk '{print $2}'
jq < manifest.json .

# Create the new image and replace the old one.
tar czvf new-image.tar.gz */layer.tar *.json
mv new-image.tar.gz "$BASE_IMAGE_FILE"
