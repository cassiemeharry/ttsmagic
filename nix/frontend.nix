{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
, nodejs ? pkgs.nodejs-12_x
}:

let
  rust-system = import ./rust.nix { inherit sources; targets = []; };
  rust-wasm = import ./rust.nix { inherit sources; targets = [ "wasm32-unknown-unknown" ]; };
  naersk-system = pkgs.callPackage sources.naersk {
    rustc = rust-system;
    cargo = rust-system;
  };
  naersk-wasm = pkgs.callPackage sources.naersk {
    rustc = rust-wasm;
    cargo = rust-wasm;
  };
  wasm-bindgen-locked-source = pkgs.stdenv.mkDerivation {
    # This whole derivation is only necessary because wasm-bindgen doens't
    # provide a Cargo.lock. I've generated it manually and vendored it alongside
    # this file.
    name = "wasm-bindgen-locked-source";
    version = "0.2.60";
    buildInputs = [ pkgs.stdenv ];
    src = pkgs.fetchFromGitHub {
      owner = "rustwasm";
      repo = "wasm-bindgen";
      rev = "0.2.60";
      sha256 = "1jr4v5y9hbkyg8gjkr3qc2qxwhyagfs8q3y3z248mr1919mcas8h";
    };
    buildPhase = ''
      runHook preBuild
      set -x
      cp "${./wasm-bindgen-v0.2.60-Cargo.lock}" Cargo.lock
      set +x
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      cp -R . "$out"
      runHook postInstall
    '';
  };
  wasm-bindgen = naersk-system.buildPackage {
    name = "wasm-bindgen";
    version = "0.2.60";
    src = wasm-bindgen-locked-source;
    buildInputs = [ pkgs.libressl pkgs.pkg-config ];
    cargoBuildOptions = orig:
      orig ++ [ "--package" "wasm-bindgen-cli" ];
    compressTarget = false;
    singleStep = true;
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

    ls -la .
    ls -la ${src}/ttsmagic-frontend/.cargo
    ls -l target/release
    ls -l target/wasm32-unknown-unknown/release/

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
