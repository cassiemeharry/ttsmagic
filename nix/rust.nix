{ sources ? import ./sources.nix }:

let
  pkgs = import sources.nixpkgs {
    overlays = [ (import sources.nixpkgs-mozilla) ];
  };
  channel = pkgs.rustChannelOfTargets "nightly" "2020-03-20" [];
in
channel
