{ pkgs, tqlModule, tqlPackage }:

# Outcome-focused integration check: an agent can make repeated MCP requests
# while qBittorrent sits behind an authentication-rate-limiting gateway.
# Every requested torrent must arrive, and both long-running services must
# remain available. The test intentionally does not inspect tql's session
# implementation.

let
  qbtPort = 8082;
  qbtProxyPort = 8083;
  mcpPort = 7878;
  requestCount = 8;
  qbtPasswordHash =
    ''@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)'';

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-mcp-sustained-use";

  nodes.machine = { pkgs, ... }: {
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
          url = "http://127.0.0.1:${toString qbtProxyPort}";
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

    # Model a common deployment boundary: authentication is protected more
    # aggressively than ordinary API traffic. One login per minute is allowed;
    # torrent operations remain unrestricted.
    services.nginx = {
      enable = true;
      appendHttpConfig = ''
        limit_req_zone $binary_remote_addr zone=qbt_login:10m rate=1r/m;
      '';
      virtualHosts."qbt-gateway" = {
        listen = [{
          addr = "127.0.0.1";
          port = qbtProxyPort;
        }];
        locations."/api/v2/auth/login" = {
          proxyPass = "http://127.0.0.1:${toString qbtPort}";
          extraConfig = ''
            limit_req zone=qbt_login;
          '';
        };
        locations."/".proxyPass = "http://127.0.0.1:${toString qbtPort}";
      };
    };

    environment.systemPackages = [ pkgs.curl pkgs.jq pkgs.mktorrent ];
  };

  testScript = ''
    import json

    start_all()
    machine.wait_for_unit("qbittorrent.service")
    machine.wait_for_open_port(${toString qbtPort})
    machine.wait_for_unit("nginx.service")
    machine.wait_for_open_port(${toString qbtProxyPort})
    machine.wait_for_unit("tql-mcp.service")
    machine.wait_for_open_port(${toString mcpPort})

    initial_pid = machine.succeed(
        "systemctl show --property MainPID --value tql-mcp.service"
    ).strip()
    assert initial_pid != "0", initial_pid

    def rpc(payload):
        machine.succeed(
            "cat > /tmp/rpc.json <<'EOF'\n"
            + json.dumps(payload)
            + "\nEOF\n"
        )
        output = machine.succeed(
            "curl -fsS -X POST "
            "-H 'Content-Type: application/json' "
            "--data-binary @/tmp/rpc.json "
            "http://127.0.0.1:${toString mcpPort}/"
        )
        return json.loads(output)

    def add_torrent(index):
        machine.succeed(
            f"printf 'tql-mcp-sustained-{index}\\n' > /var/lib/tql/payload-{index}.txt"
        )
        machine.succeed(
            f"mktorrent -a http://tracker.invalid/announce "
            f"-o /var/lib/tql/sample-{index}.torrent "
            f"/var/lib/tql/payload-{index}.txt"
        )
        machine.succeed(
            f"chown tql:tql /var/lib/tql/sample-{index}.torrent"
        )
        response = rpc({
            "jsonrpc": "2.0",
            "id": index + 1,
            "method": "tools/call",
            "params": {
                "name": "tracker.example.add",
                "arguments": {
                    "input": {
                        "url": f"https://example.org/t/{index}",
                        "categories": ["Books/Sustained"],
                        "author": f"Agent{index}",
                    },
                    "source": {
                        "kind": "file",
                        "path": f"/var/lib/tql/sample-{index}.torrent",
                    },
                },
            },
        })
        result = response["result"]
        assert result.get("isError") is False, response
        ack = json.loads(result["content"][0]["text"])
        assert ack["ok"] is True, ack
        assert ack["info_hash"], ack
        return ack

    # The first request establishes whatever authentication state tql needs.
    acknowledgements = [add_torrent(0)]

    # Prove that the deployment constraint is active. Another authentication
    # attempt through the gateway must be rejected, while ordinary authenticated
    # MCP work below must continue to succeed.
    rejected_status = machine.succeed(
        "curl -sS -o /tmp/rejected-login -w '%{http_code}' "
        "--data 'username=admin&password=adminadmin' "
        "http://127.0.0.1:${toString qbtProxyPort}/api/v2/auth/login"
    ).strip()
    assert rejected_status == "503", rejected_status

    for index in range(1, ${toString requestCount}):
        acknowledgements.append(add_torrent(index))
        machine.succeed(
            "curl -fsS http://127.0.0.1:${toString mcpPort}/health | jq -e '.ok == true'"
        )

    ping = rpc({"jsonrpc": "2.0", "id": 100, "method": "ping"})
    assert ping["result"] == {}, ping
    machine.succeed("systemctl is-active --quiet tql-mcp.service")
    machine.succeed("systemctl is-active --quiet qbittorrent.service")
    final_pid = machine.succeed(
        "systemctl show --property MainPID --value tql-mcp.service"
    ).strip()
    assert final_pid == initial_pid, (initial_pid, final_pid)
    assert machine.succeed(
        "systemctl show --property NRestarts --value tql-mcp.service"
    ).strip() == "0"

    # Verify the world-visible result through qBittorrent's real API, bypassing
    # only the deliberately constrained authentication gateway.
    machine.succeed(
        "status=$(curl -fsS -o /tmp/qbt-login-body -w '%{http_code}' "
        "--cookie-jar /tmp/qbt.jar "
        "--data 'username=admin&password=adminadmin' "
        "http://127.0.0.1:${toString qbtPort}/api/v2/auth/login); "
        "if test \"$status\" = 204; then "
        "test ! -s /tmp/qbt-login-body; "
        "else "
        "test \"$status\" = 200 && test \"$(cat /tmp/qbt-login-body)\" = 'Ok.'; "
        "fi"
    )
    machine.wait_until_succeeds(
        "curl -fsS --cookie /tmp/qbt.jar "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info "
        "| jq -e 'length == ${toString requestCount}'",
        timeout=30,
    )
    torrents = json.loads(machine.succeed(
        "curl -fsS --cookie /tmp/qbt.jar "
        "http://127.0.0.1:${toString qbtPort}/api/v2/torrents/info"
    ))
    by_hash = {torrent["hash"].lower(): torrent for torrent in torrents}
    assert len(by_hash) == ${toString requestCount}, torrents

    for index, ack in enumerate(acknowledgements):
        torrent = by_hash[ack["info_hash"].lower()]
        assert torrent["category"] == "example.org", torrent
        tags = {tag.strip() for tag in torrent["tags"].split(",")}
        assert f"link:Books/Sustained/Agent{index}" in tags, torrent
        assert f"link:_authors/Agent{index}" in tags, torrent
  '';
}
