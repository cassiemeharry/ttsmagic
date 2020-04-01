{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
}:

let
  rust = import ./rust.nix { inherit sources; };
  naersk = pkgs.callPackage sources.naersk {
    rustc = rust;
    cargo = rust;
  };
  filterFunc = path: type:
    type != "directory"
      || (
        builtins.baseNameOf path != "files"
        && builtins.baseNameOf path != "live_site"
        && builtins.baseNameOf path != "target"
      );
  src = builtins.filterSource filterFunc ../.;
in naersk.buildPackage {
  inherit src;
  remapPathPrefix = true;
  buildInputs = [pkgs.pkg-config pkgs.openssl];
}
