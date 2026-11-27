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
        # The NixOS module evaluates to a unit with the expected command line; the credential
        # is wired into [device], AMD cards get their nodes and groups, the inputs' ports open.
        module-eval =
          let
            eval =
              tb:
              (lib.nixosSystem {
                inherit system;
                modules = [
                  self.nixosModules.default
                  {
                    boot.isContainer = true;
                    system.stateVersion = "26.05";
                    services.totalblonder = { enable = true; } // tb;
                  }
                ];
              }).config;
            plain = eval { settings.video.device = "ref"; };
            device = {
              base_url = "http://192.0.2.1";
              rf_module = [ ];
            };
            creds = eval {
              deviceCredentialsFile = "/run/secrets/eqam";
              settings = {
                video.device = "ref";
                inherit device;
              };
            };
            own = eval {
              deviceCredentialsFile = "/run/secrets/eqam";
              settings = {
                video.device = "ref";
                device = device // { credentials_file = "/etc/eqam"; };
              };
            };
            amd = eval { settings.video.device = "hip"; };
            open = eval {
              openFirewall = true;
              settings = {
                video.device = "ref";
                input = [
                  { id = "a"; kind = "rtmp"; bind = "0.0.0.0:1935"; }
                  { id = "b"; kind = "ts_udp"; bind = "[::]:5004"; }
                  { id = "c"; kind = "file"; path = "/x.ts"; }
                ];
              };
            };
            svc = c: c.systemd.services.totalblonder.serviceConfig;
            conf = c: lib.last (lib.splitString " " (svc c).ExecStart);
            check = cond: msg: lib.optionalString (!cond) "echo FAIL: ${msg}; exit 1";
          in
          pkgs.runCommand "module-eval" { } ''
            echo ${lib.escapeShellArg (svc plain).ExecStart} | grep -q -- '--config /nix/store/.*totalblonder.toml'
            grep -q 'credentials_file = "/run/credentials/totalblonder.service/eqam"' ${conf creds}
            grep -q 'credentials_file = "/etc/eqam"' ${conf own}
            ! grep -q credentials_file ${conf plain}
            ${check (lib.elem "/dev/kfd rw" (svc amd).DeviceAllow) "AMD: /dev/kfd"}
            ${check (lib.elem "render" (svc amd).SupplementaryGroups) "AMD: render group"}
            ${check (lib.elem "/dev/nvidia0 rw" (svc plain).DeviceAllow) "NVIDIA nodes by default"}
            ${check (lib.elem 1935 open.networking.firewall.allowedTCPPorts) "RTMP port"}
            ${check (lib.elem 5004 open.networking.firewall.allowedUDPPorts) "TS/UDP port"}
            ${check (!lib.elem 1935 plain.networking.firewall.allowedTCPPorts) "closed by default"}
            echo ${lib.escapeShellArg plain.systemd.services.totalblonder.environment.TB_AVWORKER} | grep -q tb-avworker
            touch $out
          '';
      };

      # services.totalblonder (nix/module.nix).
      nixosModules.default = import ./nix/module.nix { packages = self.packages; };

      packages.${system} = repo.flake.packages // {
        default = ws.packages.tb-m2venc;
        kernels-device = kernels.deviceImage;
        kernels-device-hip = kernels.deviceImageHip;
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
          exec ${repo.apps."default/${triple}/tb-device-gpu/test/plumbing".program} --include-ignored --nocapture "$@"
        '';
        # The protocol-cost bench of the CUDA adapter (release build), on the card.
        gpu-bench = script "gpu-bench" ''
          exec ${
            ws.target.${triple}.variants.default.benches.tb-device-gpu."bench/protocol"
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
