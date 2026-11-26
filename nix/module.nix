# services.totalblonder: the server as a hardened systemd unit (SPEC §17, M6).
#
# The decoder worker (tb-avworker) confines itself further inside its own namespaces, so the
# unit must leave user, mount, PID, network, IPC and UTS namespaces available to it
# (RestrictNamespaces stays off); everything else is shut.
{ packages }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.totalblonder;
  toml = pkgs.formats.toml { };
  configFile =
    if cfg.configFile != null then cfg.configFile else toml.generate "totalblonder.toml" cfg.settings;
in
{
  options.services.totalblonder = {
    enable = lib.mkEnableOption "the totalblonder MPTS server";
    package = lib.mkOption {
      type = lib.types.package;
      default = packages.${pkgs.stdenv.hostPlatform.system}.totalblonder;
      description = "The server package.";
    };
    settings = lib.mkOption {
      type = toml.type;
      default = { };
      description = ''
        The configuration, as the TOML file takes it
        (crates/totalblonder/totalblonder.example.toml lists every key). Ignored when
        `configFile` is set.
      '';
    };
    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "A configuration file to use instead of `settings`.";
    };
    deviceCredentialsFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        A file with the modulator's user name and password on two lines, passed to the unit as
        the systemd credential `eqam` (readable as $CREDENTIALS_DIRECTORY/eqam; set
        `device.credentials_file` to "/run/credentials/totalblonder.service/eqam").
      '';
    };
    gpu = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Give the unit the NVIDIA device nodes (video.device = \"cuda\").";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.totalblonder = {
      description = "totalblonder MPTS server";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "totalblonder"} --config ${configFile}";
        Restart = "always";
        RestartSec = 1;
        DynamicUser = true;
        StateDirectory = "totalblonder";
        LoadCredential = lib.optional (cfg.deviceCredentialsFile != null) "eqam:${cfg.deviceCredentialsFile}";
        # The CUDA adapter pins its host arena.
        LimitMEMLOCK = "infinity";
        # Hardening.
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        LockPersonality = true;
        RestrictSUIDSGID = true;
        RemoveIPC = true;
        SystemCallArchitectures = "native";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
          "AF_NETLINK"
        ];
        UMask = "0077";
        PrivateDevices = !cfg.gpu;
        DeviceAllow = lib.optionals cfg.gpu [
          "/dev/nvidiactl rw"
          "/dev/nvidia0 rw"
          "/dev/nvidia-uvm rw"
          "/dev/nvidia-uvm-tools rw"
          "/dev/nvidia-modeset rw"
        ];
      };
      environment = lib.optionalAttrs (packages.${pkgs.stdenv.hostPlatform.system} ? tb-avworker) {
        TB_AVWORKER = lib.getExe' packages.${pkgs.stdenv.hostPlatform.system}.tb-avworker "tb-avworker";
      };
    };
  };
}
