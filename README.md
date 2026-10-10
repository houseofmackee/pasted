# Pasted

A fast, private clipboard manager for macOS, written in Rust. Inspired by [Paste](https://pasteapp.io), with history import from an existing Paste installation.

Everything stays on your Mac: no accounts and no telemetry, with optional encryption at rest. The only network access is fetching link previews, which you can turn off.

## Features

- Unlimited clipboard history with a visual shelf at the bottom of the screen (default shortcut ⇧⌘V)
- Text, rich text, links, images, files, colors and code, each with its own card; rich text keeps its formatting (from RTF, or HTML through a built-in converter that never loads remote content)
- Instant search with filters: `type:link`, `app:safari`, `after:2025-01-01`, `before:today`, `board:work`, and negation such as `-type:image`
- Text inside images is searchable (on-device OCR with Apple Vision)
- Link previews with page title, image and site icon, also searchable; only public http(s) URLs are fetched, never local or private network addresses
- Paste As (⌥Return or right-click): case changes, trimming, single line, tracking-free links, pretty or minified JSON, sorted or unique lines, URL encoding, and Markdown as formatted text; only options that change the text are shown
- Optional automatic cleanup on every paste: trim whitespace and remove link tracking parameters (both off by default)
- Snippet placeholders in pinboard items, expanded when pasting (see below)
- Rules that expire or pin new copies automatically (see below)
- Statistics window: items added per day and by time of day, types, top apps and most pasted items, over 7, 30 or 90 days, in light and dark mode
- Export to a restorable ZIP backup, JSON or CSV
- Pinboards with colors, plus a Paste Stack (⇧⌘C) for pasting several items in order
- Privacy: ignores password managers and concealed clipboard content, skips text that looks like API keys, private keys, tokens or card numbers, supports custom ignore patterns, and hides the shelf from screen sharing
- Import history, pinboards, OCR text and shortcuts from Paste
- A command line interface for scripts

## Build and run

Requires macOS 14 or later and a Rust toolchain.

```sh
cargo run --release            # run from the terminal
scripts/bundle.sh              # build and ad-hoc sign dist/Pasted.app
open dist/Pasted.app
```

Pasting directly into other apps needs Accessibility permission (System Settings > Privacy & Security > Accessibility). Grant it to `Pasted.app`; when running through `cargo run`, the permission applies to your terminal instead. Without it, choosing an item still puts it on the clipboard so you can press ⌘V yourself.

`launch_at_login` only works when running `Pasted.app`.

## Shelf shortcuts

| Key | Action |
| --- | --- |
| ← → | Select previous / next item |
| Return | Paste |
| ⇧ Return | Paste as plain text |
| ⌥ Return | Paste As: choose a cleanup |
| ⌘1 to ⌘9 | Paste the first nine items |
| Space (empty search) or ⌘Y | Preview |
| ⌘C | Copy without pasting |
| ⌘E / ⌘R | Edit text / rename |
| ⌘⌫ | Delete (or remove from the current pinboard) |
| ⌘O | Open link or show file in Finder |
| Tab, ⇧Tab, ⌘← ⌘→ | Switch between Clipboard and pinboards |
| ⌘N | New pinboard |
| Esc | Close preview, clear search, close shelf |

Right-click a card for more actions, including pinning. Drag cards into other apps.

## Snippets

Items on a pinboard can contain placeholders that are filled in when you paste them. Clipboard history is never expanded, and only the names below are placeholders, so code or JSON with braces is left alone.

| Placeholder | Pastes |
| --- | --- |
| `{date}` `{time}` `{datetime}` | current date and time; add a strftime format, for example `{date:%d/%m/%Y}` |
| `{weekday}` `{month}` `{year}` | parts of the date |
| `{date+7d}` `{weekday-1w}` | offsets in days (`d`) or weeks (`w`) |
| `{clipboard}` | the text currently on the clipboard |
| `{uuid}` | a random UUID |
| `{cursor}` | where the cursor is left after pasting |
| `{input:Name}` | asked for in a dialog when you paste |

Write `\{date}` to paste the braces literally. To paste a snippet without expanding it, use Paste As > Without Placeholders. Expansion can be turned off in Settings > Pasting.

## Rules

Rules run on every new copy. Each has optional conditions (source app, item types, a regular expression) that must all match, and one or both actions: remove the item from history after a while, or pin it to a pinboard (created if needed). Pinned items never expire. Manage rules in Settings > Rules (⌘N adds one) or in the settings file:

```toml
[[rules]]
name = "Terminal copies are temporary"
app = "Terminal"                # bundle id or app name
expire_after = "1h"             # 30s, 15m, 1h, 2d, 1w

[[rules]]
name = "Tickets"
matches = "(?i)\\bJIRA-\\d+"
pin_to = "Tickets"
```

Cards show when an item will expire. `pasted rules` lists rules and flags invalid ones; `pasted rules test "text" --app Terminal` shows which would match.

## Command line

```sh
pasted list -n 20                 # recent history
pasted search type:link github    # same syntax as the shelf
pasted show 42                    # print an item's text
pasted copy 42 [--plain]          # put an item back on the clipboard
pasted copy 42 --as snake         # copy with a cleanup applied
pbpaste | pasted transform clean-links   # cleanups work on stdin too (--list shows all)
echo 'Due {date+7d}' | pasted expand --set Name=Ada   # expand snippet placeholders
echo hi | pasted add              # save text
pasted pin 42 Snippets            # pin (creates the pinboard if needed)
pasted board list
pasted import paste --dry-run     # preview an import from Paste
pasted open                       # show the shelf of the running app
pasted settings --tab privacy     # show a Settings tab
pasted preview <url>              # fetch a link preview (debugging)
pasted export backup.zip         # also .json or .csv; --board and --query filter
pasted import archive backup.zip # restore a backup
pasted encryption [status|enable|disable]
pasted stats [--days 30]          # summary in the terminal; --show opens the Statistics window
pasted paths
```

Use `--json` with `list` and `search` for machine readable output.

## Importing from Paste

Choose **Import from Paste…** in the menu bar menu, or run `pasted import paste`. On first launch with an empty history, Pasted offers the import automatically.

Link previews Paste already stored (titles, images, icons) come along, so those links are not fetched again.

Imports copy history as it is by default. To leave sensitive items behind, choose what to skip in Settings > Data (or the `[import]` section of the settings file): items from ignored apps, API keys and tokens, text matching your ignore patterns, and short snippets that mention a password. Each check is optional and off by default; already imported items are never removed.

The importer copies Paste's database to a temporary directory and reads the copy, so Paste's own files are never modified. Running it again only adds items that are new since the last import. History retention and shortcuts are taken from Paste's settings. Quit Paste first for the most complete import.

## Encryption at rest

Turn it on in Settings > Privacy > Encryption, or with `pasted encryption enable` while the app is not running (`status` and `disable` work the same way). Pasted then:

- creates a random key in your login Keychain (item `io.pasted.app`),
- encrypts the history database with SQLCipher (AES-256, using macOS CommonCrypto),
- encrypts each stored image and file with AES-256-GCM under a name that does not reveal its content.

This keeps other apps, other user accounts and backups (Time Machine, synced folders) from reading your history. Know the limits: if the Keychain item is deleted the history cannot be recovered; exports and backups you make are not encrypted; FileVault already protects the disk while the Mac is off; and unencrypted copies from before you turned it on may remain in free disk space until it is reused.

## Export and backup

`pasted export` (or Settings > Data > Export) writes:

- **`.zip`**: a full backup with every image, file and format, pinboards, OCR text and link previews. Restore it with `pasted import archive <file>` or Settings > Data > Restore Backup; items you already have are skipped, so restoring twice is harmless.
- **`.json`**: the same details without images or files, for scripts.
- **`.csv`**: one row per item for spreadsheets.

Limit an export with `--board NAME` or `--query "type:link after:2026-09-01"`. Restored history older than your retention setting is removed at the next cleanup, as with imports.

## Settings

Choose **Settings…** in the menu bar menu or press ⌘, in the shelf. The window has four tabs:

- **General**: history retention, size limit, sounds, launch at login, link previews
- **Pasting**: direct pasting, plain text default, automatic cleanup, Accessibility status
- **Shortcuts**: click a field and press a new combination (global shortcuts are paused while recording)
- **Privacy**: confidential content, secret detection, screen sharing visibility, apps to ignore, ignore patterns
- **Data**: storage statistics, Paste import, clear history, data folder

Changes apply immediately and are saved to `~/Library/Application Support/Pasted/settings.toml`. The file can also be edited by hand; Pasted notices the change within a couple of seconds.

```toml
[general]
history_retention = "month"     # day, week, month, year, forever
max_item_size_mb = 64
paste_plain_text_by_default = false
paste_directly = true
sounds = false
launch_at_login = false
link_previews = true

[shortcuts]
open_shelf = "shift+cmd+v"
paste_stack = "shift+cmd+c"
next_pinboard = "cmd+right"
previous_pinboard = "cmd+left"

[privacy]
ignore_concealed = true
ignore_secrets = true
excluded_apps = ["com.1password.1password", "..."]
ignore_patterns = []
visible_during_screen_sharing = false

[pasting]                       # automatic cleanup on paste, off by default
trim_whitespace = false
clean_links = false
expand_placeholders = true      # snippets on pinboards only

[import]                        # optional privacy checks for imports, all off by default
skip_excluded_apps = false
skip_secrets = false
skip_ignore_patterns = false
skip_password_mentions = false
```

Set `PASTED_HOME` to use a different data directory (useful for testing). `PASTED_LOG=pasted=debug` enables verbose logging.

## Development

```sh
cargo test
cargo clippy --all-targets
```

See [PLAN.md](PLAN.md) for the roadmap.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
