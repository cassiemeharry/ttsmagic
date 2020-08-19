{ system ? builtins.currentSystem }:

let
  sources = import ./sources.nix;
  pkgs = import sources.nixpkgs {};
  app = import ./ttsmagic.nix { inherit sources; };
  secrets = pkgs.stdenv.mkDerivation {
    name = "ttsmagic-secrets";
    phases = "installPhase";
    installPhase = ''
      mkdir -p $out/etc/ttsmagic/
      cp ${../secrets.toml} $out/etc/ttsmagic/secrets.toml
    '';
  };
in pkgs.dockerTools.buildLayeredImage {
  name = "cassiemeharry/ttsmagic";
  tag = "latest";
  contents = [ app pkgs.dumb-init pkgs.cacert pkgs.busybox secrets ];
  created = "now";

  extraCommands = "mkdir -m 1777 tmp";

  config = {
    Entrypoint = [ "/bin/dumb-init" "--" ];
    Cmd = [ "/bin/ttsmagic-server server" ];
    Env = [ "HOST=0.0.0.0" "PORT=8000" "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" ];
    WorkingDir = "/ttsmagic";
  };
}
