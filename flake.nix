{
  description = "do-next — pick your next Jira task & manage it from the terminal";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
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

        packages.default = (pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        }).buildRustPackage {
          pname = "do-next";
          version = "0.1.0";

          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [
            pkgs.pkg-config # needed at build time to locate dbus
          ];

          buildInputs = [
            pkgs.dbus # required by the sync-secret-service keyring backend
          ];

          meta = {
            description = "Pick your next Jira task from the terminal";
            mainProgram = "do-next";
          };
        };

        devShells.default = pkgs.mkShell {
          # Pulls in nativeBuildInputs + buildInputs from the package above
          inputsFrom = [ self.packages.${system}.default ];
          packages = [
            rustToolchain
            pkgs.cargo-msrv
            pkgs.rust-analyzer
          ];
        };
      }
    );
}
