{
  description = "do-next — pick your next Jira task & manage it from the terminal";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        craneLib = crane.mkLib pkgs;
        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.dbus ];
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      in
      {
        checks = {
          fmt = craneLib.cargoFmt { inherit src; };
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "-- -W clippy::pedantic -W clippy::nursery -W clippy::unwrap_used";
          });
          test = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
          });
        };

        packages.default = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          meta = {
            description = "Pick your next Jira task from the terminal";
            mainProgram = "do-next";
          };
        });

        devShells.default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = [
            pkgs.cargo-msrv
            pkgs.rust-analyzer
          ];
        };
      }
    );
}
