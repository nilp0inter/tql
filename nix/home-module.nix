{ config, lib, pkgs, ... }:

let
  cfg = config.services.tql;
  tomlFormat = pkgs.formats.toml { };
  configFile =
    if cfg.configFile != null
    then cfg.configFile
    else tomlFormat.generate "tql-config.toml" cfg.settings;

  binPath = "${cfg.package}/bin/tql";

  # Home Manager units are user-scoped — none of the NixOS systemd-hardening
  # knobs (ProtectSystem, StateDirectory, etc.) apply here. The user already
  # owns the process, so we keep the unit definition minimal and rely on the
  # surrounding user session for isolation.
  baseService = extra: lib.recursiveUpdate {
    Unit = {
      Description = "tql user service";
      After = [ "network-online.target" ];
      Wants = [ "network-online.target" ];
    };
    Service = {
      Environment = [ "TQL_CONFIG=${toString configFile}" ];
      Restart = "on-failure";
      RestartSec = 5;
    } // lib.optionalAttrs (cfg.environmentFile != null) {
      EnvironmentFile = toString cfg.environmentFile;
    };
    Install = { WantedBy = [ "default.target" ]; };
  } extra;

  oneshotService = extra: lib.recursiveUpdate (baseService {
    Service = { Type = "oneshot"; Restart = lib.mkForce "no"; };
    Install = lib.mkForce { };
  }) extra;

in {
  options.services.tql = {
    enable = lib.mkEnableOption "tql — Tracker-Qualified Layout (user scope)";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.tql or (throw "services.tql.package must be set (e.g. inputs.tql.packages.\${system}.default)");
      description = "The tql package to use.";
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
        DESIGN.md §11. Ignored when
        {option}`services.tql.configFile` is set.
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to a systemd EnvironmentFile holding secrets referenced by
        tql config (qBittorrent password, tracker passkeys, API keys,
        Telegram token, …). Loaded into every tql user unit.
      '';
    };

    api = {
      enable = lib.mkEnableOption "the tql REST API user service (tql api)";
      extraArgs = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Extra CLI args for `tql api`.";
      };
    };

    mcp = {
      enable = lib.mkEnableOption "the tql MCP HTTP user service (tql mcp --http)";
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

    home.packages = [ cfg.package ];

    systemd.user.services = lib.mkMerge [
      (lib.mkIf cfg.api.enable {
        tql-api = baseService {
          Unit.Description = "tql REST API (user)";
          Service.ExecStart = lib.escapeShellArgs ([ binPath "api" ] ++ cfg.api.extraArgs);
        };
      })
      (lib.mkIf cfg.mcp.enable {
        tql-mcp = baseService {
          Unit.Description = "tql MCP HTTP server (user)";
          Service.ExecStart = lib.escapeShellArgs ([ binPath "mcp" "--http" cfg.mcp.listenAddress ] ++ cfg.mcp.extraArgs);
        };
      })
      (lib.mkIf cfg.reconcile.enable {
        tql-reconcile = oneshotService {
          Unit.Description = "tql reconcile (user)";
          Service.ExecStart = lib.escapeShellArgs ([ binPath "reconcile" ] ++ cfg.reconcile.extraArgs);
        };
      })
      (lib.mkIf cfg.notifyFlush.enable {
        tql-notify-flush = oneshotService {
          Unit.Description = "tql notify-flush (user)";
          Service.ExecStart = lib.escapeShellArgs ([ binPath "notify-flush" ] ++ cfg.notifyFlush.extraArgs);
        };
      })
    ];

    systemd.user.timers = lib.mkMerge [
      (lib.mkIf cfg.reconcile.enable {
        tql-reconcile = {
          Unit.Description = "Periodic tql reconcile (user)";
          Timer = {
            OnCalendar = cfg.reconcile.onCalendar;
            Persistent = true;
            Unit = "tql-reconcile.service";
          };
          Install.WantedBy = [ "timers.target" ];
        };
      })
      (lib.mkIf cfg.notifyFlush.enable {
        tql-notify-flush = {
          Unit.Description = "Periodic tql notify-flush (user)";
          Timer = {
            OnCalendar = cfg.notifyFlush.onCalendar;
            Persistent = true;
            Unit = "tql-notify-flush.service";
          };
          Install.WantedBy = [ "timers.target" ];
        };
      })
    ];
  };
}
