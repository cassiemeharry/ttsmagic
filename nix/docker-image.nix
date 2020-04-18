{ system ? builtins.currentSystem }:

let
  sources = import ./sources.nix;
  pkgs = import sources.nixpkgs {};
  app = import ./ttsmagic.nix { inherit sources; };
in pkgs.dockerTools.buildLayeredImage {
  name = "cassiemeharry/ttsmagic";
  tag = "latest";
  contents = [ app pkgs.dumb-init pkgs.cacert pkgs.busybox ];

  extraCommands = "mkdir -m 1777 tmp";

  config = {
    Entrypoint = [ "/bin/dumb-init" "--" ];
    Cmd = [ "/bin/ttsmagic server" ];
    Env = [ "HOST=0.0.0.0" "PORT=8000" "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" ];
    WorkingDir = "/ttsmagic";
  };
}
