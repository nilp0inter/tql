{ pkgs, tqlModule, tqlPackage }:

pkgs.testers.runNixOSTest {
  name = "tql-module";

  nodes.machine = { config, lib, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" ];
      settings = {
        paths = {
          seed_root = "/var/lib/tql/seed";
          library_root = "/var/lib/tql/library";
          trackers_root = "/var/lib/tql/trackers";
        };
        api.addr = "127.0.0.1:8080";
      };
    };

    # tql's systemd unit has ProtectSystem=strict + StateDirectory=tql, so
    # /var/lib/tql is writable. Pre-create the seed/library/trackers subdirs
    # before the unit starts.
    systemd.tmpfiles.rules = [
      "d /var/lib/tql/seed     0755 tql tql -"
      "d /var/lib/tql/library  0755 tql tql -"
      "d /var/lib/tql/trackers 0755 tql tql -"
    ];

    # Test-only conveniences.
    environment.systemPackages = [ pkgs.curl ];
  };

  testScript = ''
    start_all()
    machine.wait_for_unit("tql-api.service")
    machine.wait_for_open_port(8080)
    machine.succeed("curl -fsS http://127.0.0.1:8080/health | grep -q ok")
    # /trackers should return [] (empty trackers_root, no auth configured).
    machine.succeed("curl -fsS http://127.0.0.1:8080/trackers | grep -q '\\[\\]'")
    # The packaged binary is on PATH for system administrators.
    machine.succeed("tql --help | grep -q 'Usage:'")
  '';
}
