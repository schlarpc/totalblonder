{
  description = "totalblonder: MPEG transport-stream server for an EQAM-420B, and its CUDA MPEG-2 encoder";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # One derivation per Cargo unit. Not following our nixpkgs: breakbulk's own tools then stay
    # the same when nixpkgs is updated (breakbulk docs, "Following your nixpkgs").
    breakbulk.url = "github:schlarpc/breakbulk";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      breakbulk,
      rust-overlay,
    }:
    let
      system = "x86_64-linux";
      inherit (nixpkgs) lib;
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
        # The CUDA toolkit is unfree. Allowed here, by license, for this flake only; no global
        # setting is needed on any machine.
        config.allowUnfreePredicate =
          pkg:
          lib.all (l: (l.free or false) || (l.shortName or "") == "CUDA EULA") (
            lib.toList (pkg.meta.license or [ ])
          );
      };
      kernels = import ./nix/kernels.nix {
        inherit pkgs;
        root = ./.;
      };
      repo = breakbulk.lib.mkRepository {
        inherit pkgs;
        root = self;
        modules = [
          ./nix/rust.nix
          { checks = kernels.checks; }
        ];
        specialArgs = { inherit kernels; };
      };
      ws = repo.workspaces.default;
      gate = import ./nix/gate.nix {
        inherit pkgs lib;
        inherit (kernels) cuda nvflags;
        root = ./.;
        tb-m2venc = ws.packages.tb-m2venc;
        tb-pipe = ws.packages.tb-pipe;
        tb-m2v-verify = ws.packages.tb-m2v-verify;
      };
      verify = import ./nix/verify.nix {
        inherit pkgs lib;
        root = ./.;
        tb-m2venc = ws.packages.tb-m2venc;
        tb-m2v-verify = ws.packages.tb-m2v-verify;
      };
      tsverify = import ./nix/ts-verify.nix {
        inherit pkgs lib;
        root = ./.;
        tb-ts-verify = ws.packages.tb-ts-verify;
      };
      triple = pkgs.stdenv.hostPlatform.rust.rustcTarget;
      script = name: text: {
        type = "app";
        program = lib.getExe (pkgs.writeShellScriptBin name text);
        meta.description = name;
      };
    in
    {
      # Every sandbox-safe check: each product, each test binary's run (nextest), doctests,
      # clippy with warnings denied, rustfmt, and the kernel builds.
      checks.${system} = repo.flake.checks // gate.checks // verify.checks // tsverify.checks // {
        # The NixOS module evaluates to a unit with the expected command line.
        module-eval =
          let
            sys = lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  boot.isContainer = true;
                  system.stateVersion = "26.05";
                  services.totalblonder = {
                    enable = true;
                    settings.video.device = "ref";
                  };
                }
              ];
            };
            unit = sys.config.systemd.services.totalblonder;
          in
          pkgs.runCommand "module-eval" { } ''
            echo ${lib.escapeShellArg unit.serviceConfig.ExecStart} | grep -q -- '--config /nix/store/.*totalblonder.toml'
            touch $out
          '';
      };

      # services.totalblonder (nix/module.nix).
      nixosModules.default = import ./nix/module.nix { packages = self.packages; };

      packages.${system} = repo.flake.packages // {
        default = ws.packages.tb-m2venc;
        kernels-device = kernels.deviceImage;
        kernels-ref = kernels.refLib;
        abi-header = kernels.abiHeader;
        gate-tools = gate.tools;
        gpucomp = gate.gpucomp;
        # Nsight Systems and Compute of the pinned toolkit (tools/gate/nsys_*.sh, tools/gate/ncu_profile.sh)
        nsys = kernels.cuda.nsight_systems;
        ncu = kernels.cuda.nsight_compute;
      };

      apps.${system} = repo.flake.apps // gate.apps // verify.apps // {
        # GPU tests: run on the machine with the card (nixark). They are test apps of
        # breakbulk under their long names; these are the short ones.
        gpu-plumbing = script "gpu-plumbing" ''
          exec ${repo.apps."default/${triple}/tb-device-cuda/test/plumbing".program} --include-ignored --nocapture "$@"
        '';
        # The protocol-cost bench of the CUDA adapter (release build), on the card.
        gpu-bench = script "gpu-bench" ''
          exec ${
            ws.target.${triple}.variants.default.benches.tb-device-cuda."bench/protocol"
          }/bin/protocol "$@"
        '';
        # Regenerate the committed copy of the kernel ABI header.
        abi-header = script "abi-header" ''
          set -euo pipefail
          install -m 644 ${kernels.abiHeader} crates/tb-kernel-abi/kernels/tb_kernel_abi.h
          echo "wrote crates/tb-kernel-abi/kernels/tb_kernel_abi.h"
        '';
      };

      devShells.${system}.default = ws.devShell.overrideAttrs (old: {
        nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
          pkgs.cargo-nextest
          pkgs.rust-cbindgen
        ];
      });
    };
}
