{ pkgs, tqlHomeModule, tqlPackage }:

# Evaluates the Home Manager module under a stubbed module system (no
# dependency on the real home-manager flake) and asserts that the rendered
# user units contain the expected ExecStart lines. This is a pure eval
# check — no VM, no user session — so it runs everywhere flake checks do.

let
  lib = pkgs.lib;

  # Stub the option namespaces the HM module writes into (home.*,
  # systemd.user.*) so we can evalModules without pulling in home-manager.
  stub = { lib, ... }: {
    options = {
      home.packages = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = [ ];
      };
      systemd.user.services = lib.mkOption {
        type = lib.types.attrsOf (lib.types.attrsOf lib.types.anything);
        default = { };
      };
      systemd.user.timers = lib.mkOption {
        type = lib.types.attrsOf (lib.types.attrsOf lib.types.anything);
        default = { };
      };
      assertions = lib.mkOption {
        type = lib.types.listOf lib.types.anything;
        default = [ ];
      };
    };
  };

  evaluated = lib.evalModules {
    modules = [
      stub
      tqlHomeModule
      {
        _module.args = { inherit pkgs; };
        services.tql = {
          enable = true;
          package = tqlPackage;
          api.enable = true;
          mcp.enable = true;
          reconcile.enable = true;
          notifyFlush.enable = true;
          settings = {
            paths = {
              seed_root = "/tmp/seed";
              library_root = "/tmp/library";
              trackers_root = "/tmp/trackers";
            };
            api.addr = "127.0.0.1:8080";
          };
        };
      }
    ];
  };

  cfg = evaluated.config;
  units = cfg.systemd.user.services;
  timers = cfg.systemd.user.timers;
  bin = "${tqlPackage}/bin/tql";

  # `builtins.match` (used by containsSubstr) rejects strings with store-path
  # context. Drop context for the substring searches — we are not building
  # the strings, just inspecting them at eval time.
  strip = builtins.unsafeDiscardStringContext;
  containsSubstr = needle: haystack:
    lib.hasInfix (strip needle) (strip haystack);

  expect = label: cond:
    if cond then null
    else throw "home-module check failed: ${label}";

  checks = [
    (expect "tql-api ExecStart contains binary path"
      (containsSubstr bin units.tql-api.Service.ExecStart))
    (expect "tql-mcp ExecStart mentions --http"
      (containsSubstr "--http" units.tql-mcp.Service.ExecStart))
    (expect "tql-reconcile is oneshot"
      (units.tql-reconcile.Service.Type or null == "oneshot"))
    (expect "tql-notify-flush is oneshot"
      (units.tql-notify-flush.Service.Type or null == "oneshot"))
    (expect "reconcile timer OnCalendar set"
      (timers.tql-reconcile.Timer.OnCalendar == "*:0/10"))
    (expect "notify-flush timer OnCalendar set"
      (timers.tql-notify-flush.Timer.OnCalendar == "*:0/2"))
    (expect "TQL_CONFIG env is set on tql-api"
      (lib.any (e: lib.hasPrefix "TQL_CONFIG=" e)
        units.tql-api.Service.Environment))
    (expect "tql package is added to home.packages"
      (lib.elem tqlPackage cfg.home.packages))
  ];

in pkgs.runCommand "tql-home-module-eval" { } (
  # Force evaluation of every assertion at build-time. deepSeq walks the
  # list and forces each element; a failed `expect` throws, which aborts
  # the eval before runCommand is constructed.
  builtins.deepSeq checks ''
    echo ok > $out
  ''
)
