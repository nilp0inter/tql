{ pkgs, tqlModule, tqlPackage }:

# End-to-end check for the §15 notify pipeline.
#
# Boots tql in a NixOS VM, points the Telegram drainer at a local Python
# HTTP sink (no real Bot API), enqueues one notify event for category
# `example.org`, runs `tql notify-flush --force`, and asserts the sink
# captured a `POST /botFAKE/sendMessage` whose JSON body contains the
# example tracker's HTML markup (📦 prefix, `<b>` name, `<code>` category,
# ↑ link-add glyph) — i.e. the per-tracker template override from
# Leg 58d, end-to-end through the real systemd-invoked drainer.

let
  sinkPort = 9099;

  exampleTracker = pkgs.runCommand "tql-tracker-example" { } ''
    mkdir -p $out
    cp -r ${../trackers/example}/. $out/
  '';

  # Tiny HTTP sink: accept any POST, write the body verbatim to
  # /tmp/captured.json, reply with the canonical Bot API success shape.
  sinkScript = pkgs.writeText "tql-notify-sink.py" ''
    import http.server
    import socketserver

    class H(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            n = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(n)
            with open("/tmp/captured.path", "wb") as f:
                f.write(self.path.encode())
            with open("/tmp/captured.json", "wb") as f:
                f.write(body)
            resp = b'{"ok":true,"result":{}}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(resp)))
            self.end_headers()
            self.wfile.write(resp)
        def log_message(self, *_):  # quiet
            pass

    with socketserver.TCPServer(("127.0.0.1", ${toString sinkPort}), H) as srv:
        srv.serve_forever()
  '';
in
pkgs.testers.runNixOSTest {
  name = "tql-notify";

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ tqlModule ];

    services.tql = {
      enable = true;
      package = tqlPackage;
      api.enable = true;
      readWritePaths = [ "/var/lib/tql" ];
      environmentFile = pkgs.writeText "tql-notify-env" ''
        TQL_TG_TOKEN=FAKE
      '';
      settings = {
        paths = {
          seed_root = "/var/lib/tql/seed";
          library_root = "/var/lib/tql/library";
          trackers_root = "/var/lib/tql/trackers";
        };
        api.addr = "127.0.0.1:8080";
        notify = {
          default = [ "telegram" ];
          telegram = {
            bot_token_env = "TQL_TG_TOKEN";
            chat_id = "-1001234567890";
            parse_mode = "HTML";
            base_url = "http://127.0.0.1:${toString sinkPort}";
          };
        };
      };
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/tql/seed                       0755 tql tql -"
      "d /var/lib/tql/library                    0755 tql tql -"
      "d /var/lib/tql/library/.metadata          0755 tql tql -"
      "d /var/lib/tql/trackers                   0755 tql tql -"
      "L+ /var/lib/tql/trackers/example          -    -   -   - ${exampleTracker}"
    ];

    systemd.services.tql-notify-sink = {
      description = "Local HTTP sink standing in for the Telegram Bot API";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        ExecStart = "${pkgs.python3}/bin/python3 ${sinkScript}";
        Restart = "on-failure";
        RestartSec = 1;
      };
    };

    environment.systemPackages = [ pkgs.jq ];
  };

  testScript = ''
    import json

    start_all()
    machine.wait_for_unit("tql-notify-sink.service")
    machine.wait_for_open_port(${toString sinkPort})
    machine.wait_for_unit("tql-api.service")

    # Sanity: example tracker bundle (with its notify.hbs override) is in place.
    machine.succeed("test -f /var/lib/tql/trackers/example/manifest.toml")
    machine.succeed("test -f /var/lib/tql/trackers/example/notify.hbs")

    cfg = machine.succeed(
        "systemctl show -p Environment tql-api.service "
        "| sed -n 's/.*TQL_CONFIG=\\([^ ]*\\).*/\\1/p'"
    ).strip()
    print("config:", cfg)

    # Enqueue one event directly into the spool. JSONL: one line per event.
    # Schema mirrors src/notify/mod.rs::Event (schema_version=1).
    event = {
        "schema_version": 1,
        "ts": "2026-05-12T00:00:00Z",
        "info_hash_v1": "deadbeef",
        "name": "Example Torrent",
        "category": "example.org",
        "link_sites_added": ["Sci/Knuth"],
        "link_sites_removed": [],
        "warnings": [],
    }
    machine.succeed(
        "install -d -o tql -g tql -m 0755 /var/lib/tql/library/.metadata"
    )
    machine.succeed(
        "echo {!r} | install -o tql -g tql -m 0644 /dev/stdin "
        "/var/lib/tql/library/.metadata/notify.spool".format(json.dumps(event))
    )

    env_file = "${pkgs.writeText "tql-notify-flush-env" ''
      TQL_TG_TOKEN=FAKE
    ''}"

    # Force-flush under the tql user via systemd-run so the EnvironmentFile is honored.
    out = machine.succeed(
        "systemd-run --pipe --wait --quiet --unit=tql-notify-test "
        "--uid=tql --gid=tql "
        f"--property=EnvironmentFile={env_file} "
        f"tql notify-flush --config {cfg} --force --json"
    )
    print("notify-flush:", out)
    outcome = json.loads(out)
    assert outcome["outcome"] == "ok", outcome
    assert outcome["sent"] == 1, outcome
    assert outcome["requeued"] == 0, outcome

    # The sink should have captured one POST to /botFAKE/sendMessage.
    path = machine.succeed("cat /tmp/captured.path").strip()
    assert path == "/botFAKE/sendMessage", path

    body_raw = machine.succeed("cat /tmp/captured.json")
    print("captured body:", body_raw)
    body = json.loads(body_raw)
    assert body["chat_id"] == "-1001234567890", body
    assert body["parse_mode"] == "HTML", body
    text = body["text"]
    # The example tracker's notify.hbs decorates the default shape() output
    # with a 📦 prefix, an inline <code> category, and ↑/↓ link-diff glyphs.
    assert "📦" in text, text
    assert "<b>Example Torrent</b>" in text, text
    assert "<code>example.org</code>" in text, text
    assert "↑ Sci/Knuth" in text, text

    # Spool should be drained (one event sent, none requeued).
    machine.succeed("test ! -e /var/lib/tql/library/.metadata/notify.spool")
  '';
}
