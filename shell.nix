let
  sources = import ./nix/sources.nix;
  rust = import ./nix/rust.nix {
    inherit sources;
    targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
  };
  pkgs = import sources.nixpkgs {};
  naersk = pkgs.callPackage sources.naersk {
    rustc = rust;
    cargo = rust;
  };
  wasm-bindgen = import ./nix/wasm-bindgen.nix { inherit sources; };
  cargo-udeps = naersk.buildPackage {
    name = "cargo-udeps";
    version = "0.1.11";
    src = pkgs.fetchFromGitHub {
      owner = "est31";
      repo = "cargo-udeps";
      rev = "v0.1.11";
      sha256 = "1drz0slv33p4spm52sb5lnmpb83q8l7k3cvp0zcsinbjv8glvvnv";
    };
    buildInputs = [ pkgs.libressl pkgs.pkg-config pkgs.zlib ];
  };
in
pkgs.mkShell {
  shellHook = ''
    set -o allexport
    [[ -f .env ]] && source .env
    set +o allexport
  '';
  buildInputs = [
    cargo-udeps
    pkgs.binaryen
    pkgs.colordiff
    pkgs.file
    pkgs.inotify-tools
    pkgs.linuxPackages.perf
    pkgs.openssl.dev
    pkgs.perf-tools
    pkgs.pkg-config
    pkgs.python3
    pkgs.ripgrep
    pkgs.sshfs
    pkgs.time
    pkgs.valgrind
    rust
    wasm-bindgen
  ];
}
