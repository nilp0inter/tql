{ config, lib, pkgs, ... }:

let
  cfg = config.services.tql;
  tomlFormat = pkgs.formats.toml { };
  configFile =
    if cfg.configFile != null
    then cfg.configFile
    else tomlFormat.generate "tql-config.toml" cfg.settings;

  binPath = "${cfg.package}/bin/tql";

  # Common hardening for every unit.
  hardening = {
    NoNewPrivileges = true;
    PrivateTmp = true;
    ProtectSystem = "strict";
    ProtectHome = true;
    ProtectKernelTunables = true;
    ProtectKernelModules = true;
    ProtectKernelLogs = true;
    ProtectControlGroups = true;
    RestrictNamespaces = true;
    RestrictSUIDSGID = true;
    LockPersonality = true;
    MemoryDenyWriteExecute = true;
    SystemCallArchitectures = "native";
    # tql talks to qBittorrent / Plex / Jellyfin / Telegram over the net.
    RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" ];
  };

  baseService = extra: lib.recursiveUpdate {
    serviceConfig = hardening // {
      User = cfg.user;
      Group = cfg.group;
      EnvironmentFile = lib.optional (cfg.environmentFile != null) cfg.environmentFile;
      ReadWritePaths = cfg.readWritePaths;
      StateDirectory = "tql";
    };
    environment.TQL_CONFIG = toString configFile;
  } extra;

in {
  options.services.tql = {
    enable = lib.mkEnableOption "tql — Tracker-Qualified Layout";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.tql or (throw "services.tql.package must be set (e.g. inputs.tql.packages.\${system}.default)");
      description = "The tql package to use.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "tql";
      description = "User that runs tql services.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "tql";
      description = "Group that runs tql services.";
    };

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to a tql config TOML file. Mutually exclusive with
        {option}`services.tql.settings`. When null, the file is rendered
        from {option}`services.tql.settings`.
      '';
    };

    settings = lib.mkOption {
      type = tomlFormat.type;
      default = { };
      description = ''
        tql configuration as a Nix attrset, rendered to TOML. See
        DESIGN.md §11 for the schema. Ignored when
        {option}`services.tql.configFile` is set.
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to a systemd EnvironmentFile holding secrets referenced by
        tql config (qBittorrent password, tracker passkeys, API keys,
        Telegram token, …). Loaded into every tql unit.
      '';
    };

    readWritePaths = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "/srv/torrents" "/srv/library" ];
      description = ''
        Filesystem paths the tql units need to write to (seed_root,
        library_root, metadata_dir). Required because
        `ProtectSystem=strict` is enabled.
      '';
    };

    api = {
      enable = lib.mkEnableOption "the tql REST API systemd service (tql api)";
      extraArgs = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Extra CLI args for `tql api`.";
      };
    };

    mcp = {
      enable = lib.mkEnableOption "the tql MCP HTTP service (tql mcp --http)";
      listenAddress = lib.mkOption {
        type = lib.types.str;
        default = "127.0.0.1:7878";
        description = "Address to bind tql mcp's HTTP transport.";
      };
      extraArgs = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Extra CLI args for `tql mcp`.";
      };
    };

    reconcile = {
      enable = lib.mkEnableOption "periodic `tql reconcile` (safety net)";
      onCalendar = lib.mkOption {
        type = lib.types.str;
        default = "*:0/10";
        description = "systemd OnCalendar spec; default = every 10 minutes.";
      };
      extraArgs = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Extra CLI args for `tql reconcile`.";
      };
    };

    notifyFlush = {
      enable = lib.mkEnableOption "periodic `tql notify-flush` (drain notify spool)";
      onCalendar = lib.mkOption {
        type = lib.types.str;
        default = "*:0/2";
        description = "systemd OnCalendar spec; default = every 2 minutes.";
      };
      extraArgs = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Extra CLI args for `tql notify-flush`.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.configFile == null || cfg.settings == { };
        message = "services.tql.configFile and services.tql.settings are mutually exclusive.";
      }
    ];

    users.users = lib.mkIf (cfg.user == "tql") {
      tql = {
        isSystemUser = true;
        group = cfg.group;
        description = "tql service user";
      };
    };

    users.groups = lib.mkIf (cfg.group == "tql") {
      tql = { };
    };

    environment.systemPackages = [ cfg.package ];

    systemd.services.tql-api = lib.mkIf cfg.api.enable (baseService {
      description = "tql REST API";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        ExecStart = lib.escapeShellArgs ([ binPath "api" ] ++ cfg.api.extraArgs);
        Restart = "on-failure";
        RestartSec = 5;
      };
    });

    systemd.services.tql-mcp = lib.mkIf cfg.mcp.enable (baseService {
      description = "tql MCP HTTP server";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        ExecStart = lib.escapeShellArgs ([ binPath "mcp" "--http" cfg.mcp.listenAddress ] ++ cfg.mcp.extraArgs);
        Restart = "on-failure";
        RestartSec = 5;
      };
    });

    systemd.services.tql-reconcile = lib.mkIf cfg.reconcile.enable (baseService {
      description = "tql reconcile (safety net)";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.escapeShellArgs ([ binPath "reconcile" ] ++ cfg.reconcile.extraArgs);
      };
    });

    systemd.timers.tql-reconcile = lib.mkIf cfg.reconcile.enable {
      description = "Periodic tql reconcile";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.reconcile.onCalendar;
        Persistent = true;
        Unit = "tql-reconcile.service";
      };
    };

    systemd.services.tql-notify-flush = lib.mkIf cfg.notifyFlush.enable (baseService {
      description = "tql notify-flush (drain notify spool)";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.escapeShellArgs ([ binPath "notify-flush" ] ++ cfg.notifyFlush.extraArgs);
      };
    });

    systemd.timers.tql-notify-flush = lib.mkIf cfg.notifyFlush.enable {
      description = "Periodic tql notify-flush";
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.notifyFlush.onCalendar;
        Persistent = true;
        Unit = "tql-notify-flush.service";
      };
    };
  };
}
