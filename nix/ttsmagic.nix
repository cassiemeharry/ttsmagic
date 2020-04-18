{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
}:

let
  rust = import ./rust.nix { inherit sources; };
  naersk = pkgs.callPackage sources.naersk {
    rustc = rust;
    cargo = rust;
  };
  srcFilter = path: _type:
    (builtins.match "\.nix$" path) == null;
  src = pkgs.nix-gitignore.gitignoreFilterSource srcFilter [] ../.;
in naersk.buildPackage {
  inherit src;
  remapPathPrefix = true;
  buildInputs = [ pkgs.cacert pkgs.pkg-config pkgs.openssl ];
}
