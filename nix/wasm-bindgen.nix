{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
}:

let
  rust-system = import ./rust.nix { inherit sources; targets = []; };
  wasm-bindgen-version = "0.2.71";
  lock-file = ./wasm-bindgen-v0.2.71-Cargo.lock;
  download-sha256 = "1zdap7k727ry548f1ml846c7llyln92v2i1cdi9brafihp2d6h9v";
  naersk-system = pkgs.callPackage sources.naersk {
    rustc = rust-system;
    cargo = rust-system;
  };
  wasm-bindgen-locked-source = pkgs.stdenv.mkDerivation {
    # This whole derivation is only necessary because wasm-bindgen doens't
    # provide a Cargo.lock. I've generated it manually and vendored it alongside
    # this file.
    name = "wasm-bindgen-locked-source";
    version = wasm-bindgen-version;
    buildInputs = [ pkgs.stdenv ];
    src = pkgs.fetchFromGitHub {
      owner = "rustwasm";
      repo = "wasm-bindgen";
      rev = wasm-bindgen-version;
      sha256 = download-sha256;
    };
    buildPhase = ''
      runHook preBuild
      set -x
      cp "${lock-file}" Cargo.lock
      set +x
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      cp -R . "$out"
      runHook postInstall
    '';
  };
in
  naersk-system.buildPackage {
    name = "wasm-bindgen";
    version = wasm-bindgen-version;
    src = wasm-bindgen-locked-source;
    buildInputs = [ pkgs.libressl_3_0 pkgs.pkg-config ];
    cargoBuildOptions = orig:
      orig ++ [ "--package" "wasm-bindgen-cli" ];
    compressTarget = false;
    singleStep = true;
  }
