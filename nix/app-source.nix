{ sources ? import ./sources.nix
, pkgs ? import sources.nixpkgs {}
, selection
}:

let
  srcFilter = path: _type:
    (builtins.match "\.nix$" path) == null;
  allSrc = pkgs.stdenv.mkDerivation {
    # This doesn't actually do any network requests or anything, but I'd like
    # the hash to be calculated from the *output* of the derivation to minimize
    # spurious rebuilds, which is only done happens for "impure" builds.
    __impure = true;
    name = "ttsmagic-src-full";
    src = pkgs.nix-gitignore.gitignoreFilterSource srcFilter [ "*.nix" "ttsmagic-server/static/ttsmagic_frontend*" ] ../.;
    buildPhase = "true";
    installPhase = ''
      mkdir "$out"
      cp -r ttsmagic-{frontend,types,s3,server} Cargo.{toml,lock} "$out"
    '';
  };

  toTOML = pkgs.callPackage "${sources.naersk.outPath}/builtins/to-toml.nix" {};

  opts =
    if selection == "frontend" then
      { remove = [ "ttsmagic-s3" "ttsmagic-server" ]; }
    else if selection == "server" then
      { remove = [ "ttsmagic-frontend" ]; }
    else if selection == "types" then
      { remove = [ "ttsmagic-frontend" "ttsmagic-s3" "ttsmagic-server" ]; }
    else if selection == null then
      { remove = []; }
    else
      throw "Invalid selection ${selection}, expected either \"frontend\", \"server\", or null";

  fakeCargoToml = toRemove: (
    let
      filename = "${allSrc}/${toRemove}/Cargo.toml";
      contents = builtins.readFile filename;
      parsed = builtins.fromTOML contents;
      replacement = { package = parsed.package; };
    in
      toTOML replacement
  );

  buildPhaseRemoveCrate = toRemove: ''
    rm -r ${toRemove}
    mkdir ${toRemove} ${toRemove}/src
    echo '${fakeCargoToml toRemove}' > ${toRemove}/Cargo.toml
    echo "" > ${toRemove}/src/lib.rs
  '';

  buildPhase = builtins.concatStringsSep
    "\n\n"
    (builtins.map buildPhaseRemoveCrate opts.remove);

  drvAttrs = {
    name = "ttsmagic-${selection}-src";
    src = allSrc;
    toRemove = opts.remove;
    buildPhase = if buildPhase == "" then "true" else buildPhase;
    installPhase = ''
      cp -r "$(pwd)" "$out"
    '';
  };
in pkgs.stdenv.mkDerivation drvAttrs
