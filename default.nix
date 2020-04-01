{ system ? builtins.currentSystem }:

let
  sources = import ./nix/sources.nix;
  pkgs = import sources.nixpkgs {};
  app = import ./nix/ttsmagic.nix { inherit sources; };
in pkgs.dockerTools.buildLayeredImage {
  name = "cassiemeharry/ttsmagic";
  tag = "latest";
  contents = [ app ];

  config = {
    Cmd = [ "/bin/ttsmagic server" ];
    Env = [ "HOST=0.0.0.0" "PORT=8000" ];
    WorkingDir = "/ttsmagic";
  };
}
