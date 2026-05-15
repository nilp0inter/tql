# TODO

## PR #4 — `notify-flush` Telegram handling

- [x] Enforce the 4096 UTF-16 Telegram limit on the **final** grouped message output in `format_message_grouped`, not only per rendered run.
- [x] Make truncation parse-mode safe (HTML and MarkdownV2) so truncation cannot produce malformed payloads.
- [x] Add tests for grouped-message total length enforcement (multi-run case).
- [x] Add tests for truncation correctness/safety in HTML and MarkdownV2 modes.
- [ ] Resolve CI `action_required` runs for branch `copilot/fix-telegram-message-length-error`. *(requires authenticated `gh` — run `gh auth login` and then `gh run list --branch copilot/fix-telegram-message-length-error` to approve / rerun.)*
- [ ] Mark PR #4 as ready for review after code and CI are complete. *(requires authenticated `gh`: `gh pr ready 4`.)*
