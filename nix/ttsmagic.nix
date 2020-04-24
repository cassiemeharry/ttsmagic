{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
}:

let
  rust = import ./rust.nix { inherit sources; };
  naersk = pkgs.callPackage sources.naersk {
    rustc = rust;
    cargo = rust;
  };
  frontend = import ./frontend.nix { inherit sources; };
  src-no-frontend = import ./app-source.nix { inherit sources; selection = "server"; };
  src = pkgs.stdenv.mkDerivation {
    name = "ttsmagic-server-src";
    src = src-no-frontend;
    buildPhase = ''
      ls -l . ${frontend} ttsmagic-server/
      cp ${frontend}/* ttsmagic-server/static/
    '';
    installPhase = ''
      cp -r "$(pwd)" "$out"
    '';
  };
in naersk.buildPackage {
  name = "ttsmagic-server";
  inherit src;
  remapPathPrefix = true;
  buildInputs = [ pkgs.cacert pkgs.pkg-config pkgs.openssl ];
}
