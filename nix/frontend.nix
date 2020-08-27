{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
, nodejs ? pkgs.nodejs-12_x
}:

let
  wasm-bindgen = import ./wasm-bindgen.nix { inherit sources; };
  rust-wasm = import ./rust.nix { inherit sources; targets = [ "wasm32-unknown-unknown" ]; };
  naersk-wasm = pkgs.callPackage sources.naersk {
    rustc = rust-wasm;
    cargo = rust-wasm;
  };
  src = import ./app-source.nix { inherit sources; selection = "frontend"; };
  frontend-wasm = naersk-wasm.buildPackage {
    inherit src;
    # src = src + /ttsmagic-frontend;
    name = "ttsmagic-frontend-wasm";
    CARGO_INCREMENTAL = "0";
    RUST_LOG = "info";
    cargoBuildOptions = orig:
      [ "--target" "wasm32-unknown-unknown" "--package" "ttsmagic-frontend" ] ++ orig;
    compressTarget = false;
    copyBins = false;
    copyTarget = true;
    dontFixup = true;
    remapPathPrefix = false;
  };
in pkgs.stdenv.mkDerivation {
  name = "ttsmagic-frontend-wasm-bindgen";
  src = frontend-wasm;
  buildInputs = [
    pkgs.binaryen
    wasm-bindgen
  ];
  buildPhase = ''
    runHook preBuild

    wasm=target/wasm32-unknown-unknown/release/ttsmagic_frontend.wasm
    wasm-bindgen --target web --no-typescript --out-dir=$(pwd) "$wasm"

    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall

    pwd
    mkdir "$out"
    cp ttsmagic_frontend* "$out"

    runHook postInstall
  '';
}
