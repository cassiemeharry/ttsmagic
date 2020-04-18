let
  sources = import ./nix/sources.nix;
  rust = import ./nix/rust.nix { inherit sources; };
  pkgs = import sources.nixpkgs {};
in
pkgs.mkShell {
  buildInputs = [
    pkgs.colordiff
    pkgs.elm2nix
    pkgs.elmPackages.elm
    pkgs.elmPackages.elm-format
    pkgs.file
    pkgs.inotify-tools
    pkgs.pkg-config
    pkgs.python3
    pkgs.openssl.dev
    pkgs.sshfs
    pkgs.time
    rust
  ];
}
