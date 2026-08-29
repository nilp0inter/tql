{ pkgs, tqlModule, tqlPackage }:

# End-to-end check: drive the `tql api` REST surface against a live
# qbittorrent-nox. Submits a `.torrent` via
# `POST /trackers/example/add` and asserts the ack shape matches
# `tql cli` plus the torrent landed in qBittorrent with the
# classifier-derived category + tags.
#
# Complements `nix/test-cli.nix`, which exercises the same workflow
# through the CLI surface. This one covers DESIGN §13's REST transport.

let
  qbtPort = 8082;
  apiPort = 8080;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-api-e2e";

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
        api.addr = "127.0.0.1:${toString apiPort}";
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
    machine.wait_for_open_port(${toString apiPort})

    # Liveness: /health is unauthenticated.
    machine.succeed(
        "curl -fsS http://127.0.0.1:${toString apiPort}/health | grep -q ok"
    )

    # /trackers should list the seeded example bundle.
    trackers_raw = machine.succeed(
        "curl -fsS http://127.0.0.1:${toString apiPort}/trackers"
    )
    print("trackers:", trackers_raw)

    import json
    trackers = json.loads(trackers_raw)
    names = {t["name"] for t in trackers["trackers"]} \
        if isinstance(trackers, dict) and "trackers" in trackers \
        else {t["name"] for t in trackers}
    assert "example" in names, trackers

    # Build a tiny .torrent payload and stage it where tql can read it.
    # tql-api runs under `tql` user with ProtectSystem=strict; /tmp is
    # private to the unit. Use /var/lib/tql which is in readWritePaths.
    machine.succeed("echo 'tql-api-e2e-fixture' > /var/lib/tql/payload.txt")
    machine.succeed("chown tql:tql /var/lib/tql/payload.txt")
    machine.succeed(
        "mktorrent -a 'http://tracker.invalid/announce' "
        "-o /var/lib/tql/sample.torrent /var/lib/tql/payload.txt"
    )
    machine.succeed("chown tql:tql /var/lib/tql/sample.torrent")

    # Submit via the REST API. Source kind=file points at the path the
    # tql-api process can read.
    body = json.dumps({
        "input": {
            "url": "https://example.org/t/456",
            "categories": ["Books/Technical"],
            "author": "Ada",
        },
        "source": {"kind": "file", "path": "/var/lib/tql/sample.torrent"},
    })
    out = machine.succeed(
        "curl -fsS -X POST "
        "-H 'Content-Type: application/json' "
        f"-d '{body}' "
        "http://127.0.0.1:${toString apiPort}/trackers/example/add"
    )
    print("api ack:", out)

    ack = json.loads(out)
    assert ack["ok"] is True, ack
    assert ack["tracker"] == "example", ack
    assert ack["category"] == "example.org", ack
    expected_link = "link:Books/Technical/Ada"
    assert expected_link in ack["link_tags"], ack
    assert "link:_authors/Ada" in ack["link_tags"], ack

    # Confirm via qBittorrent's WebUI that the torrent is registered with
    # the classifier-derived category + tags.
    machine.succeed(
        "status=$(curl -fsS -o /tmp/qbt-login-body -w '%{http_code}' --cookie-jar /tmp/c.jar "
        "--data 'username=admin&password=adminadmin' "
        "http://127.0.0.1:${toString qbtPort}/api/v2/auth/login); "
        "if test \"$status\" = 204; then "
        "test ! -s /tmp/qbt-login-body; "
        "else "
        "test \"$status\" = 200 && test \"$(cat /tmp/qbt-login-body)\" = 'Ok.'; "
        "fi"
    )

    machine.wait_until_succeeds(
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
