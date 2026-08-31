{
  description = "do-next — pick your next Jira task & manage it from the terminal";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        craneLib = crane.mkLib pkgs;
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type: (pkgs.lib.hasSuffix ".html" path) || (craneLib.filterCargoSources path type);
          name = "source";
        };
        commonArgs = {
          inherit src;
          # pkg-config is for aws-lc-sys (rustls); the keyring's Secret Service
          # client is pure Rust (zbus), so there is no libdbus to link.
          nativeBuildInputs = [ pkgs.pkg-config ];
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        # The suite drives a real `git` (update checks) and builds a reqwest
        # client, which refuses to start without a system CA store. Kept out of
        # commonArgs so the deps derivation stays cached.
        testArgs = {
          nativeCheckInputs = [
            pkgs.cacert
            pkgs.git
          ];
          # The oauth callback tests bind a listener on 127.0.0.1; the darwin
          # sandbox denies even loopback networking without this attribute.
          __darwinAllowLocalNetworking = true;
        };
      in
      {
        checks = {
          fmt = craneLib.cargoFmt { inherit src; };
          # The lint set lives in Cargo.toml's [lints] table; `deny` applies to
          # local packages only, so dependency warnings stay non-fatal and the
          # shared build cache is not invalidated.
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "";
              CARGO_BUILD_WARNINGS = "deny";
            }
          );
          test = craneLib.cargoTest (
            commonArgs
            // testArgs
            // {
              inherit cargoArtifacts;
            }
          );
        };

        packages.default = craneLib.buildPackage (
          commonArgs
          // testArgs
          // {
            inherit cargoArtifacts;
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.installShellFiles ];
            postInstall = ''
              installShellCompletion --cmd do-next \
                --bash <($out/bin/do-next completions bash) \
                --zsh  <($out/bin/do-next completions zsh) \
                --fish <($out/bin/do-next completions fish)
            '';
            meta = {
              description = "Pick your next Jira task from the terminal";
              mainProgram = "do-next";
            };
          }
        );

        devShells.default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = [
            pkgs.bacon
            pkgs.cargo-audit
            pkgs.cargo-edit
            pkgs.cargo-msrv
            pkgs.clippy
            pkgs.rust-analyzer
          ];
        };

        formatter = pkgs.nixfmt-tree;
      }
    );
}
