# Nix flake for ai-memory.
#
# Provides:
#   nix build              # → result/bin/ai-memory  (native release binary)
#   nix run . -- --version # smoke-test without installing
#   nix develop            # dev shell with Rust 1.95 (pinned)
#
# The build is self-contained: SQLite is bundled via rusqlite's `bundled`
# feature, libgit2 is vendored via git2's `vendored-libgit2` feature, and
# TLS uses rustls (webpki-roots) — no OpenSSL, no system-library hunting.
# The web crate copies the committed static/tailwind.css on ordinary builds,
# so the sandbox needs no Tailwind download or build environment override.
#
# This derivation builds the native binary without executing tests. Run the
# CLI suite from the development shell:
# `nix develop -c cargo test -p ai-memory-cli --test suite packaging::`.
# Packaging tests use isolated filesystem fixtures and Docker/Podman stubs.

{
  description = "Long-term memory for AI coding agents";

  # Inputs are pinned to explicit revisions rather than floating branches.
  #
  # A flake's value is reproducibility, and `github:NixOS/nixpkgs/nixos-unstable`
  # resolves to whatever that branch points at today, so two people — or the
  # same person a week apart — can get different builds from identical source.
  # Normally `flake.lock` handles this; pinning here achieves the same
  # determinism and keeps the tree honest for contributors who do not have
  # Nix installed and so cannot regenerate a lock.
  #
  # To update: bump these revisions deliberately, in their own commit, and
  # let the `nix` CI job prove the result still builds.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/0ae2bc1419c3f345984c2629e72e7a631820fa4d";
    flake-utils.url = "github:numtide/flake-utils/11707dc2f618dd54ca8739b309ec4fc024de578b";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/99607a06c2ea1290cd3258c11d1416dde9201f94";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Read the same toolchain file the project pins for every other CI
        # path — rust-toolchain.toml says `channel = "1.95"`.
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        rustPlatform = pkgs.makeRustPlatform {
          rustc = rust;
          cargo = rust;
        };
      in
      {
        packages.default = rustPlatform.buildRustPackage {
          pname = "ai-memory";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          # No nativeBuildInputs needed — the build is fully self-contained
          # (SQLite bundled, libgit2 vendored, rustls with webpki-roots).

          buildType = "release";

          # Run the complete workspace suite through the development shell:
          # `nix develop -c cargo test --workspace --all-targets`.
          # This derivation keeps the build and test steps separate.
          doCheck = false;

          # Install the bundled hook scripts alongside the binary,
          # mirroring what the AUR PKGBUILD does. Native binary users
          # (`ai-memory serve`, `install-hooks`) look up hooks under
          # the binary's share directory at runtime.
          #
          # `bin/ai-memory` (the Docker-wrapper shell script) is NOT
          # installed — Nix users build the native binary directly and
          # have no need for a Docker wrapper.
          postInstall = ''
            mkdir -p $out/share/ai-memory
            cp -a hooks $out/share/ai-memory/

            # Install the default config template so `ai-memory init`
            # has a known-good starting point without a network fetch.
            mkdir -p $out/etc/ai-memory
            cp crates/ai-memory-cli/templates/config.default.toml \
               $out/etc/ai-memory/config.default.toml
          '';

          meta = {
            description = "Long-term memory for AI coding agents";
            homepage = "https://github.com/akitaonrails/ai-memory";
            license = pkgs.lib.licenses.mit;
            mainProgram = "ai-memory";
          };
        };

        devShells.default = pkgs.mkShell {
          name = "ai-memory-dev";

          buildInputs = [
            rust
            pkgs.cargo-watch
          ];

          shellHook = ''
            echo ""
            echo "ai-memory dev shell — Rust $(rustc --version)"
            echo ""
            echo "  cargo build --workspace          # build"
            echo "  cargo test --workspace           # unit + integration tests"
            echo "  cargo test -p ai-memory-cli --test suite packaging::"
            echo ""
          '';
        };
      }
    );
}
