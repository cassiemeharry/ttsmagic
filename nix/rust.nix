{ sources ? import ./sources.nix
, targets ? []
}:

let
  pkgs = import sources.nixpkgs {
    overlays = [ (import sources.nixpkgs-mozilla) ];
  };
  baseChannel = pkgs.rustChannelOfTargets "nightly" "2021-03-01" targets;
  channel = baseChannel.override {
    extensions = ["rust-src" "rustfmt-preview"];
  };
in
channel
