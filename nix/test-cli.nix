{ pkgs, tqlModule, tqlPackage }:

# End-to-end check: boot a NixOS VM with a real qbittorrent-nox daemon,
# seed the bundled `example` tracker into the configured `trackers_root`,
# generate a tiny `.torrent` file via `mktorrent`, and prove `tql cli
# example ...` lands the torrent in qBittorrent with the classifier-derived
# category + tags.
#
# Complements `nix/test-qbittorrent.nix` (which only proves `doctor` can
# talk to qBittorrent). This one walks the actual user workflow:
# manifest → script → submission → qBittorrent state.

let
  qbtPort = 8082;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  # Tracker bundle from the repo root, materialized as a store path so we
  # can symlink it into the VM's trackers_root via systemd-tmpfiles.
  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-cli-e2e";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" ];
      environmentFile = pkgs.writeText "tql-env" ''
        TQL_QBIT_PASSWORD=adminadmin
      '';
      settings = {
        paths = {
          seed_root = "/var/lib/tql/seed";
          library_root = "/var/lib/tql/library";
          trackers_root = "/var/lib/tql/trackers";
        };
        api.addr = "127.0.0.1:8080";
        qbittorrent = {
          url = "http://127.0.0.1:${toString qbtPort}";
          username = "admin";
          password_env = "TQL_QBIT_PASSWORD";
        };
      };
    };

    users.users.qbt = {
      isSystemUser = true;
      group = "qbt";
      home = "/var/lib/qbt";
      createHome = true;
    };
    users.groups.qbt = { };

    environment.etc."qbt/qBittorrent.conf".text = ''
      [LegalNotice]
      Accepted=true

      [Preferences]
      WebUI\Username=admin
      WebUI\Password_PBKDF2="${qbtPasswordHash}"
      WebUI\Port=${toString qbtPort}
      WebUI\Address=127.0.0.1
      WebUI\LocalHostAuth=true
      WebUI\CSRFProtection=false
      WebUI\HostHeaderValidation=false
    '';

    systemd.tmpfiles.rules = [
      "d /var/lib/tql/seed                       0755 tql tql -"
      "d /var/lib/tql/library                    0755 tql tql -"
      "d /var/lib/tql/trackers                   0755 tql tql -"
      "L+ /var/lib/tql/trackers/example          -    -   -   - ${exampleTracker}"
      "d /var/lib/qbt/qBittorrent                0755 qbt qbt -"
      "d /var/lib/qbt/qBittorrent/config         0755 qbt qbt -"
      "C /var/lib/qbt/qBittorrent/config/qBittorrent.conf 0644 qbt qbt - /etc/qbt/qBittorrent.conf"
    ];

    systemd.services.qbittorrent = {
      description = "qBittorrent (headless)";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        User = "qbt";
        Group = "qbt";
        ExecStart = "${pkgs.qbittorrent-nox}/bin/qbittorrent-nox --profile=/var/lib/qbt --webui-port=${toString qbtPort}";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };

    environment.systemPackages = [ pkgs.curl pkgs.jq pkgs.mktorrent ];
  };

  testScript = ''
    start_all()
    machine.wait_for_unit("qbittorrent.service")
    machine.wait_for_open_port(${toString qbtPort})
    machine.wait_for_unit("tql-api.service")
    machine.wait_for_open_port(8080)

    # Sanity: trackers_root seeded with the example bundle.
    machine.succeed("test -f /var/lib/tql/trackers/example/manifest.toml")
    machine.succeed("test -f /var/lib/tql/trackers/example/classify.rhai")

    # Build a tiny .torrent file. mktorrent needs an existing payload.
    machine.succeed("echo 'tql-e2e-fixture' > /tmp/payload.txt")
    machine.succeed(
        "mktorrent -a 'http://tracker.invalid/announce' "
        "-o /tmp/sample.torrent /tmp/payload.txt"
    )

    # Submit via `tql cli example ...`. We invoke under systemd-run so
    # the password env var is loaded (same trick as test-qbittorrent.nix).
    cfg = machine.succeed(
        "systemctl show -p Environment tql-api.service "
        "| sed -n 's/.*TQL_CONFIG=\\([^ ]*\\).*/\\1/p'"
    ).strip()
    print("config:", cfg)

    out = machine.succeed(
        "systemd-run --pipe --wait --quiet "
        "--unit=tql-cli-test "
        "--property=EnvironmentFile=${pkgs.writeText "tql-env" ''
          TQL_QBIT_PASSWORD=adminadmin
        ''} "
        f"tql cli --config {cfg} example "
        "--url=https://example.org/t/123 "
        "--categories=Books/Technical "
        "--author=Ada "
        "/tmp/sample.torrent"
    )
    print("cli ack:", out)

    import json
    ack = json.loads(out)
    assert ack["ok"] is True, ack
    assert ack["tracker"] == "example", ack
    assert ack["category"] == "example.org", ack
    expected_link = "link:Books/Technical/Ada"
    assert expected_link in ack["link_tags"], ack
    assert "link:_authors/Ada" in ack["link_tags"], ack

    # Confirm via qBittorrent's WebUI that the torrent is registered with the
    # classifier-derived category + tags.
    machine.succeed(
        "curl -fsS --cookie-jar /tmp/c.jar "
        "--data 'username=admin&password=adminadmin' "
        "http://127.0.0.1:${toString qbtPort}/api/v2/auth/login | grep -q Ok"
    )

    # qBittorrent may take a moment to ingest the just-uploaded .torrent.
    info_raw = machine.wait_until_succeeds(
        "curl -fsS --cookie /tmp/c.jar "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info "
        "| jq -e 'length >= 1'",
        timeout=30,
    )
    info_raw = machine.succeed(
        "curl -fsS --cookie /tmp/c.jar "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info"
    )
    print("qbt info:", info_raw)
    torrents = json.loads(info_raw)
    assert len(torrents) >= 1, torrents
    t = torrents[0]
    assert t["category"] == "example.org", t
    tag_set = {s.strip() for s in t["tags"].split(",")}
    assert expected_link in tag_set, t
    assert "link:_authors/Ada" in tag_set, t
    assert "link:Books/Technical/Ada" in tag_set, t
  '';
}
