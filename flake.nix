{
  description = "gcli: query Grafana logs, metrics and traces from the command line";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    flake-parts.url = "github:hercules-ci/flake-parts";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
    git-hooks.url = "github:cachix/git-hooks.nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";
    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.git-hooks.flakeModule
        inputs.treefmt-nix.flakeModule
      ];
      # nixpkgs no longer supports x86_64-darwin.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      perSystem =
        {
          config,
          inputs',
          lib,
          pkgs,
          ...
        }:
        let
          inherit (pkgs.stdenv.hostPlatform) isDarwin isLinux;
          fenix = inputs'.fenix.packages;
          # `latest`, not `complete`: complete waits for every component (miri included) and can
          # trail the newest nightly by days, while the musl std below only exists as `latest`.
          # Every component used here ships in each nightly, so they always share one date.
          rust = fenix.latest;

          # Linux builds are static musl binaries, so the release artifact runs on any distribution.
          pkgsTarget = if isLinux then pkgs.pkgsStatic else pkgs;
          # Windows is cross-compiled with mingw, from any build platform.
          pkgsWindows = pkgs.pkgsCross.mingwW64;
          targetStd = p: fenix.targets.${p.stdenv.hostPlatform.rust.rustcTarget}.latest.rust-std;
          craneFor =
            p: extraStd:
            ((inputs.crane.mkLib p).overrideToolchain (
              _:
              fenix.combine (
                [
                  rust.cargo
                  rust.clippy
                  rust.rustc
                ]
                ++ extraStd
              )
            )).overrideScope
              (
                final: _: {
                  # The default writer is crane-utils, which compiles with nixpkgs' own Rust toolchain;
                  # remarshal is a small cached download.
                  writeTOML = final.writeTOMLViaRemarshal;
                }
              );
          craneLib = craneFor pkgsTarget (lib.optional isLinux (targetStd pkgsTarget));
          # Native crane pointed at mingw by hand: crane's cross mode adds wine as the test runner,
          # which macOS can't evaluate and Linux would download for a build that never runs it.
          craneWindows = craneFor pkgs [ (targetStd pkgsWindows) ];
          windowsCc = pkgsWindows.stdenv.cc;

          # Each crane derivation sees only the files it reads: docs edits rebuild none of them,
          # test edits leave the binary alone, and only a manifest change rebuilds the dependencies.
          sourceOf =
            files:
            lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions files;
            };
          manifest = [
            ./Cargo.toml
            ./Cargo.lock
            ./.cargo/config.toml
          ];

          commonArgs = {
            pname = "gcli";
            src = sourceOf (manifest ++ [ ./src ]);
            strictDeps = true;
          };
          # Dependencies compiled once for check, build and test, shared by everything below.
          cargoArtifacts = craneLib.buildDepsOnly (commonArgs // { src = sourceOf manifest; });
          testArgs = commonArgs // {
            inherit cargoArtifacts;
            # Leaf checks: nothing reads their target/, so don't archive it into the store.
            doInstallCargoArtifacts = false;
            src = sourceOf (
              manifest
              ++ [
                ./src
                (lib.fileset.fileFilter (file: file.hasExt "rs") ./tests)
              ]
            );
          };

          gcli = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              # The test suite is its own check (gcli-test).
              doCheck = false;
              # Nothing in the binary may point into /nix/store; this is what makes it portable.
              allowedReferences = [ ];
              # std links nixpkgs' libiconv on macOS; point it at the copy every Mac ships.
              postInstall = lib.optionalString isDarwin ''
                install_name_tool -change \
                  "$(otool -L $out/bin/gcli | awk '/\/nix\/store\/.*libiconv/ {print $1}')" \
                  /usr/lib/libiconv.2.dylib $out/bin/gcli
              '';
              meta = {
                description = "Command line client for querying Grafana logs, metrics and traces";
                mainProgram = "gcli";
              };
            }
          );

          windowsArgs = commonArgs // {
            pname = "gcli-windows";
            # There is no Windows host to run the suite on; this also keeps test-only crates out
            # of the dependency build.
            doCheck = false;
            # The target-prefixed mingw binutils: rustc runs dlltool for raw-dylib crates
            # (windows-sys), and the cc crate finds ar by the same prefix.
            nativeBuildInputs = [ windowsCc.bintools.bintools ];
            CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${windowsCc}/bin/${windowsCc.targetPrefix}cc";
            CC_x86_64_pc_windows_gnu = "${windowsCc}/bin/${windowsCc.targetPrefix}cc";
            # aws-lc-sys includes <sched.h>, which mingw ships with winpthreads.
            CFLAGS_x86_64_pc_windows_gnu = "-I${pkgsWindows.windows.pthreads}/include";
            # Static libgcc and winpthreads, so the exe needs none of mingw's runtime DLLs.
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-C target-feature=+crt-static -L native=${pkgsWindows.windows.pthreads}/lib";
          };
          gcli-windows = craneWindows.buildPackage (
            windowsArgs
            // {
              cargoArtifacts = craneWindows.buildDepsOnly (windowsArgs // { src = sourceOf manifest; });
              # The Windows counterpart of allowedReferences: stock Windows has no mingw DLLs.
              postInstall = ''
                if ${windowsCc.targetPrefix}objdump -p $out/bin/gcli.exe | grep -i 'DLL Name: lib'; then
                  echo "gcli.exe must not import mingw runtime DLLs" >&2
                  exit 1
                fi
              '';
            }
          );
        in
        {
          checks = {
            inherit gcli;
            gcli-clippy = craneLib.cargoClippy (
              testArgs // { cargoClippyExtraArgs = "--all-targets -- --deny warnings"; }
            );
            gcli-test = craneLib.cargoTest (
              testArgs
              // {
                # The integration suite talks to wiremock servers on loopback.
                __darwinAllowLocalNetworking = true;
                # reqwest refuses to build a client without CA certificates; the Linux sandbox has none.
                SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
              }
            );
          };
          treefmt = {
            programs.nixfmt.enable = true;
            programs.nixfmt.package = pkgs.nixfmt;
            programs.rustfmt.enable = true;
            # The dev shell's nightly rustfmt; nixpkgs' drags in its own rustc and LLVM (1.7 GiB).
            programs.rustfmt.package = rust.rustfmt;
            programs.prettier.enable = true;
          };
          pre-commit = {
            # checks.treefmt already verifies formatting; the hook's own check would repeat it.
            check.enable = false;
            settings.hooks.treefmt.enable = true;
          };
          packages.default = gcli;
          packages.gcli = gcli;
          packages.gcli-windows = gcli-windows;
          apps.default = {
            type = "app";
            program = lib.getExe gcli;
            meta.description = gcli.meta.description;
          };
          devShells.default = pkgs.mkShell {
            inherit (config.pre-commit) shellHook;
            packages =
              let
                devToolchain = rust.withComponents [
                  "cargo"
                  "clippy"
                  "rust-analyzer"
                  "rust-src"
                  "rustc"
                  "rustfmt"
                ];
              in
              [
                devToolchain
                pkgs.cargo-deny
                pkgs.cargo-edit
                pkgs.cargo-udeps
                pkgs.cargo-wizard
              ]
              ++ config.pre-commit.settings.enabledPackages;
            env.RUST_SRC_PATH = "${rust.rust-src}/lib/rustlib/src/rust/library";
          };
        };
    };
}
