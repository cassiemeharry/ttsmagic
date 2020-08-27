{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
}:

let
  rust-system = import ./rust.nix { inherit sources; targets = []; };
  naersk-system = pkgs.callPackage sources.naersk {
    rustc = rust-system;
    cargo = rust-system;
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
in
  naersk-system.buildPackage {
    name = "wasm-bindgen";
    version = "0.2.60";
    src = wasm-bindgen-locked-source;
    buildInputs = [ pkgs.libressl pkgs.pkg-config ];
    cargoBuildOptions = orig:
      orig ++ [ "--package" "wasm-bindgen-cli" ];
    compressTarget = false;
    singleStep = true;
  }
