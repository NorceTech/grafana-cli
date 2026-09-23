{
  description = "Rust development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    fenix.inputs.nixpkgs.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    flake-parts.url = "github:hercules-ci/flake-parts";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
    git-hooks.url = "github:cachix/git-hooks.nix";
    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs =
    inputs@{ self, ... }:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.git-hooks.flakeModule
        inputs.treefmt-nix.flakeModule
      ];
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      flake.overlays.fenix = inputs.fenix.overlays.default;
      perSystem =
        {
          config,
          lib,
          pkgs,
          system,
          ...
        }:
        let
          toolchain = inputs.fenix.packages.${system}.complete.toolchain;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
        in
        {
          treefmt = {
            programs.nixfmt.enable = true;
            programs.nixfmt.package = pkgs.nixfmt;
            programs.rustfmt.enable = true;
            programs.prettier.enable = true;
          };
          pre-commit.settings.hooks = {
            treefmt.enable = true;
          };
          packages.default = rustPlatform.buildRustPackage {
            pname = "gcli";
            version = (lib.importTOML ./Cargo.toml).package.version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./.cargo
                ./Cargo.lock
                ./Cargo.toml
                ./src
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            # Integration tests spawn local wiremock servers and write qa/ transcripts; run them with `cargo test` in the dev shell.
            doCheck = false;
            meta = {
              description = "Command line client for querying Grafana logs, metrics and traces";
              mainProgram = "gcli";
            };
          };
          packages.gcli = config.packages.default;
          apps.default = {
            type = "app";
            program = lib.getExe config.packages.default;
            meta.description = config.packages.default.meta.description;
          };
          devShells.default = pkgs.mkShell {
            inherit (config.pre-commit) shellHook;
            packages =
              with pkgs;
              [
                toolchain
                rust-analyzer-nightly
                cargo
                cargo-deny
                cargo-edit
                cargo-udeps
                cargo-wizard
              ]
              ++ config.pre-commit.settings.enabledPackages;

            env = {
              RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
            };
          };
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = lib.attrValues self.overlays;
          };
        };
    };
}
