{ pkgs, tqlModule, tqlPackage }:

# End-to-end check: drive `tql mcp --http` against a live qbittorrent-nox.
# Speaks JSON-RPC 2.0 over the same wire an MCP client would use:
# `initialize`, `tools/list`, then a `tools/call` for
# `tracker.example.add` that lands the .torrent in qBittorrent with the
# classifier-derived category + tags.
#
# Complements `nix/test-api.nix` (REST transport) by covering DESIGN §7's
# MCP transport end-to-end through the NixOS module's `services.tql.mcp`
# unit.

let
  qbtPort = 8082;
  mcpPort = 7878;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-mcp-e2e";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      mcp = {
        enable = true;
        listenAddress = "127.0.0.1:${toString mcpPort}";
      };
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
    machine.wait_for_unit("tql-mcp.service")
    machine.wait_for_open_port(${toString mcpPort})

    # Liveness: /health is unauthenticated.
    machine.succeed(
        "curl -fsS http://127.0.0.1:${toString mcpPort}/health | grep -q ok"
    )

    import json

    def rpc(payload):
        machine.succeed(
            "cat > /tmp/rpc.json <<'EOF'\n"
            + json.dumps(payload)
            + "\nEOF\n"
        )
        out = machine.succeed(
            "curl -fsS -X POST "
            "-H 'Content-Type: application/json' "
            "--data-binary @/tmp/rpc.json "
            "http://127.0.0.1:${toString mcpPort}/"
        )
        return json.loads(out)

    # 1) initialize handshake.
    init = rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    print("initialize:", init)
    assert init["jsonrpc"] == "2.0", init
    assert init["id"] == 1, init
    assert init["result"]["protocolVersion"] == "2024-11-05", init
    assert "tools" in init["result"]["capabilities"], init

    # 2) tools/list — the example bundle should appear as one tool.
    tools = rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    print("tools/list:", tools)
    names = {t["name"] for t in tools["result"]["tools"]}
    assert "tracker.example.add" in names, tools
    by_name = {t["name"]: t for t in tools["result"]["tools"]}
    schema = by_name["tracker.example.add"]["inputSchema"]
    assert schema["type"] == "object", schema
    assert "input" in schema["properties"], schema
    assert "source" in schema["properties"], schema

    # Build a tiny .torrent payload owned by `tql` (the unit user) so the
    # service can read it under its hardened mount namespace.
    machine.succeed("echo 'tql-mcp-e2e-fixture' > /var/lib/tql/payload.txt")
    machine.succeed("chown tql:tql /var/lib/tql/payload.txt")
    machine.succeed(
        "mktorrent -a 'http://tracker.invalid/announce' "
        "-o /var/lib/tql/sample.torrent /var/lib/tql/payload.txt"
    )
    machine.succeed("chown tql:tql /var/lib/tql/sample.torrent")

    # 3) tools/call — submit the torrent through MCP and assert the
    # classifier-derived ack matches the REST surface.
    call = rpc({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "tracker.example.add",
            "arguments": {
                "input": {
                    "url": "https://example.org/t/789",
                    "categories": ["Books/Technical"],
                    "author": "Ada",
                },
                "source": {"kind": "file", "path": "/var/lib/tql/sample.torrent"},
            },
        },
    })
    print("tools/call:", call)
    result = call["result"]
    assert result.get("isError") is False, result
    assert result["content"][0]["type"] == "text", result
    ack = json.loads(result["content"][0]["text"])
    assert ack["ok"] is True, ack
    assert ack["tracker"] == "example", ack
    assert ack["category"] == "example.org", ack
    expected_link = "link:Books/Technical/Ada"
    assert expected_link in ack["link_tags"], ack
    assert "link:_authors/Ada" in ack["link_tags"], ack

    # 4) Confirm via qBittorrent's WebUI that the torrent landed with the
    # same classifier-derived category + tags.
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

    # 5) Unknown method round-trips a JSON-RPC error (not a tool error).
    unknown = rpc({"jsonrpc": "2.0", "id": 4, "method": "does/not/exist"})
    print("unknown:", unknown)
    assert unknown["error"]["code"] == -32601, unknown
  '';
}
