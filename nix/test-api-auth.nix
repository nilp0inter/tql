{ pkgs, tqlModule, tqlPackage }:

# End-to-end check: drive the `tql api` REST surface with
# `[api].api_key_env` set, and verify the auth gate via the wire.
#
# Complements `nix/test-api.nix`, which exercises the same surface in
# open mode. This one closes the gap Leg 61 explicitly left out of
# scope: VM-level coverage of the bearer/X-Api-Key auth path under a
# hardened systemd unit.

let
  apiPort = 8080;
  apiKey = "s3cret-token";

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-api-auth-e2e";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" ];
      environmentFile = pkgs.writeText "tql-env" ''
        TQL_QBIT_PASSWORD=adminadmin
        TQL_API_KEY=${apiKey}
      '';
      settings = {
        paths = {
          seed_root = "/var/lib/tql/seed";
          library_root = "/var/lib/tql/library";
          trackers_root = "/var/lib/tql/trackers";
        };
        api = {
          addr = "127.0.0.1:${toString apiPort}";
          api_key_env = "TQL_API_KEY";
        };
        qbittorrent = {
          url = "http://127.0.0.1:65535";
          username = "admin";
          password_env = "TQL_QBIT_PASSWORD";
        };
      };
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/tql/seed                       0755 tql tql -"
      "d /var/lib/tql/library                    0755 tql tql -"
      "d /var/lib/tql/trackers                   0755 tql tql -"
      "L+ /var/lib/tql/trackers/example          -    -   -   - ${exampleTracker}"
    ];

    environment.systemPackages = [ pkgs.curl pkgs.jq ];
  };

  testScript = ''
    start_all()
    machine.wait_for_unit("tql-api.service")
    machine.wait_for_open_port(${toString apiPort})

    base = "http://127.0.0.1:${toString apiPort}"

    # /health is always unauthenticated, regardless of api_key_env.
    machine.succeed(f"curl -fsS {base}/health | grep -q ok")

    # /trackers without a key: 401.
    status = machine.succeed(
        f"curl -s -o /dev/null -w '%{{http_code}}' {base}/trackers"
    ).strip()
    assert status == "401", f"expected 401 without key, got {status}"

    # /trackers with a wrong bearer: 403.
    status = machine.succeed(
        "curl -s -o /dev/null -w '%{http_code}' "
        "-H 'Authorization: Bearer wrong' "
        f"{base}/trackers"
    ).strip()
    assert status == "403", f"expected 403 with wrong key, got {status}"

    # /trackers with a wrong X-Api-Key: 403.
    status = machine.succeed(
        "curl -s -o /dev/null -w '%{http_code}' "
        "-H 'X-Api-Key: wrong' "
        f"{base}/trackers"
    ).strip()
    assert status == "403", f"expected 403 with wrong x-api-key, got {status}"

    # /trackers with the correct bearer: 200 + example bundle present.
    import json
    out = machine.succeed(
        "curl -fsS "
        "-H 'Authorization: Bearer ${apiKey}' "
        f"{base}/trackers"
    )
    trackers = json.loads(out)
    names = {t["name"] for t in trackers["trackers"]} \
        if isinstance(trackers, dict) and "trackers" in trackers \
        else {t["name"] for t in trackers}
    assert "example" in names, trackers

    # /trackers with the correct X-Api-Key: also 200.
    out = machine.succeed(
        "curl -fsS "
        "-H 'X-Api-Key: ${apiKey}' "
        f"{base}/trackers"
    )
    json.loads(out)

    # /trackers/example/schema is gated too.
    status = machine.succeed(
        f"curl -s -o /dev/null -w '%{{http_code}}' {base}/trackers/example/schema"
    ).strip()
    assert status == "401", f"expected 401 on schema without key, got {status}"
    machine.succeed(
        "curl -fsS "
        "-H 'Authorization: Bearer ${apiKey}' "
        f"{base}/trackers/example/schema | jq -e '.title or .properties or .type'"
    )

    # /openapi.json is gated too.
    status = machine.succeed(
        f"curl -s -o /dev/null -w '%{{http_code}}' {base}/openapi.json"
    ).strip()
    assert status == "401", f"expected 401 on openapi without key, got {status}"
    machine.succeed(
        "curl -fsS "
        "-H 'Authorization: Bearer ${apiKey}' "
        f"{base}/openapi.json | jq -e '.openapi'"
    )

    # POST /trackers/example/add without a key: 401, before any
    # qbittorrent contact (URL points at an unreachable port, so a
    # bypassed gate would yield 502, not 401).
    body = json.dumps({
        "input": {
            "url": "https://example.org/t/789",
            "categories": ["Books/Technical"],
            "author": "Ada",
        },
        "source": {"kind": "url", "url": "https://example.org/missing.torrent"},
    })
    status = machine.succeed(
        "curl -s -o /dev/null -w '%{http_code}' -X POST "
        "-H 'Content-Type: application/json' "
        f"-d '{body}' {base}/trackers/example/add"
    ).strip()
    assert status == "401", f"expected 401 on add without key, got {status}"
  '';
}
