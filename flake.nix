{
  description = "Embeddable bounded-history guardian Bitcoin backend";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
    # dpc's published fork provides the Vale integration used by this project.
    flakebox.url = "github:dpc/flakebox";
    flakebox.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      flakebox,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        projectName = "fedimint-light-node";
        flakeboxLib = flakebox.lib.mkLib pkgs {
          config = {
            github.ci.buildOutputs = [ ".#ci.tests" ];
            just.rules.watch.enable = false;
            vale.enable = true;
            # Keep the checked-in recipe portable across development systems.
            just.rules.vale.content = pkgs.lib.mkForce ''
              # lint prose with Vale
              [positional-arguments]
              vale *ARGS:
                #!/usr/bin/env bash
                set -euo pipefail
                mapfile -d "" -t vale_paths < <(git ls-files -z -- '*.md')
                if [[ "''${#vale_paths[@]}" -eq 0 ]]; then
                  exit 0
                fi
                vale --config .vale.ini "$@" -- "''${vale_paths[@]}"
            '';
            # Stable nixpkgs Wild lacks flakebox's debug-compression flags.
            linker.wild.enable = false;
            linker.mold.enable = true;
            toolchain.components = [
              "rustc"
              "cargo"
              "clippy"
              "rust-analyzer"
              "rust-src"
              "rustfmt"
            ];
          };
        };
        buildSrc = flakeboxLib.source.fromPaths {
          root = ./.;
          paths = [
            ".rustfmt.toml"
            "Cargo.toml"
            "Cargo.lock"
            "src"
            "tests"
          ];
        };
        multiBuild = (flakeboxLib.craneMultiBuild { }) (
          craneLib':
          let
            craneLib = craneLib'.overrideArgs {
              pname = projectName;
              src = buildSrc;
              env.RUSTDOCFLAGS = "-D warnings";
            };
          in
          rec {
            workspaceDeps = craneLib.buildWorkspaceDepsOnly { };
            workspace = craneLib.buildWorkspace {
              cargoArtifacts = workspaceDeps;
            };
            tests = craneLib.cargoNextest {
              cargoArtifacts = workspace;
              cargoNextestExtraArgs = "--workspace";
              doInstallCargoArtifacts = false;
            };
            clippy = craneLib.cargoClippy {
              cargoArtifacts = workspaceDeps;
              doInstallCargoArtifacts = false;
            };
            cargoFmt = craneLib.cargoFmt { };
            ${projectName} = craneLib.buildPackage {
              cargoArtifacts = workspaceDeps;
            };
          }
        );
        treefmt =
          pkgs.runCommand "treefmt-check"
            {
              nativeBuildInputs = [
                pkgs.treefmt
                pkgs.nixfmt-rfc-style
                pkgs.rustfmt
                pkgs.taplo
              ];
              src = self;
            }
            ''
              cp -r $src work && chmod -R u+w work
              cd work
              treefmt --ci
              touch $out
            '';
      in
      {
        packages = {
          inherit treefmt;
          default = multiBuild.${projectName};
        };
        legacyPackages = multiBuild;
        devShells = flakeboxLib.mkShells {
          packages = [ pkgs.taplo ];
        };
      }
    );
}
