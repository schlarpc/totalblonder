# services.totalblonder: the server as a hardened systemd unit (SPEC §17, M6).
#
# The decoder worker (tb-avworker) confines itself further inside its own namespaces, so the
# unit must leave user, mount, PID, network, IPC and UTS namespaces available to it
# (RestrictNamespaces stays off); everything else is shut.
{
  packages,
  # The flake's revision, reported as service.version.
  rev ? "unknown",
}:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.totalblonder;
  toml = pkgs.formats.toml { };
  system = pkgs.stdenv.hostPlatform.system;
  # Where systemd puts the credential `eqam` (LoadCredential below).
  credentialPath = "/run/credentials/totalblonder.service/eqam";
  # [telemetry] from the module's options, under what `settings` says (settings win).
  telemetry = {
    telemetry = lib.filterAttrs (_: v: v != null) {
      inherit (cfg.telemetry) logs metrics;
      instance = config.networking.hostName;
    }
    // {
      resource."service.version" = cfg.telemetry.version;
    };
  };
  # The settings as written, with device.credentials_file filled in when the credential is
  # given through deviceCredentialsFile and the [device] section does not name one.
  withCredentials =
    if
      cfg.deviceCredentialsFile != null
      && cfg.settings ? device
      && !(cfg.settings.device ? credentials_file)
    then
      lib.recursiveUpdate cfg.settings { device.credentials_file = credentialPath; }
    else
      cfg.settings;
  settings = lib.recursiveUpdate telemetry withCredentials;
  configFile =
    if cfg.configFile != null then cfg.configFile else toml.generate "totalblonder.toml" settings;
  # The GPU's device nodes: NVIDIA's (CUDA, NVDEC), or AMD's (HIP through /dev/kfd, VA-API and
  # HIP through the DRM render nodes).
  vendor =
    if cfg.gpuVendor != null then
      cfg.gpuVendor
    else if (cfg.settings.video.device or "cuda") == "hip" then
      "amd"
    else
      "nvidia";
  gpuDevices =
    {
      nvidia = [
        "/dev/nvidiactl rw"
        "/dev/nvidia0 rw"
        "/dev/nvidia-uvm rw"
        "/dev/nvidia-uvm-tools rw"
        "/dev/nvidia-modeset rw"
      ];
      amd = [
        "/dev/kfd rw"
        "char-drm rw"
      ];
    }
    .${vendor};
  # The listeners' ports, for openFirewall: [[input]] binds of kind rtmp (TCP) and ts_udp (UDP).
  inputs = cfg.settings.input or [ ];
  portOf = i: lib.toInt (lib.last (lib.splitString ":" i.bind));
  portsOf = kind: map portOf (lib.filter (i: (i.kind or "") == kind && i ? bind) inputs);
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
        the systemd credential `eqam`. With `settings`, `device.credentials_file` points at it
        unless the [device] section names another file; with `configFile`, set it to
        "/run/credentials/totalblonder.service/eqam" yourself.
      '';
    };
    gpu = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Give the unit the GPU's device nodes (see `gpuVendor`).";
    };
    gpuVendor = lib.mkOption {
      type = lib.types.nullOr (
        lib.types.enum [
          "nvidia"
          "amd"
        ]
      );
      default = null;
      description = ''
        Whose device nodes the unit gets: NVIDIA's (`/dev/nvidia*`) or AMD's (`/dev/kfd` and
        the DRM nodes, with the `render` and `video` groups). `null`: AMD when
        `settings.video.device` is "hip", else NVIDIA.
      '';
    };
    workerPackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = packages.${system}.tb-avworker or null;
      description = "The sandboxed decoder worker (`tb-avworker`); `null` for none (no live inputs).";
    };
    telemetry = {
      logs = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://monitoring.example:9428/insert/opentelemetry/v1/logs";
        description = ''
          The OTLP/HTTP logs URL, whole (`[telemetry] logs`): log lines and wide events, among
          them one per carrier, service, input and the process every 10 s. VictoriaLogs takes
          them at `/insert/opentelemetry/v1/logs`, a collector at `/v1/logs`. `null`: not
          exported (wide events then go to the journal).
        '';
      };
      metrics = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://monitoring.example:9428/opentelemetry/v1/metrics";
        description = ''
          The OTLP/HTTP metrics URL, whole (`[telemetry] metrics`): every series of
          `/api/metrics`. VictoriaMetrics takes them at `/opentelemetry/v1/metrics`, a collector
          at `/v1/metrics`. `null`: not exported.
        '';
      };
      version = lib.mkOption {
        type = lib.types.str;
        default = rev;
        description = "The `service.version` resource attribute: by default the flake's revision.";
      };
    };
    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Open the firewall for the inputs in `settings`: the TCP port of each `rtmp` input and
        the UDP port of each `ts_udp` and `srt` input (`hls` inputs pull, and need none). The control API is never opened: it has no
        authentication.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    networking.firewall = lib.mkIf cfg.openFirewall {
      allowedTCPPorts = portsOf "rtmp";
      allowedUDPPorts = portsOf "ts_udp" ++ portsOf "srt";
    };
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
        DeviceAllow = lib.optionals cfg.gpu gpuDevices;
        # AMD's nodes belong to these groups (NVIDIA's are world-accessible)
        SupplementaryGroups = lib.optionals (cfg.gpu && vendor == "amd") [
          "render"
          "video"
        ];
      };
      environment = lib.optionalAttrs (cfg.workerPackage != null) {
        TB_AVWORKER = lib.getExe' cfg.workerPackage "tb-avworker";
      };
    };
  };
}
