# Pasted

A fast, private clipboard manager for macOS, written in Rust. Inspired by [Paste](https://pasteapp.io), with history import from an existing Paste installation.

Everything stays on your Mac: no accounts and no telemetry, with optional encryption at rest. The only network access is fetching link previews, which you can turn off.

## Features

- Unlimited clipboard history with a visual shelf at the bottom of the screen (default shortcut ⇧⌘V, or click the menu bar icon; right-click or Control-click the icon for its menu)
- Click sounds when copying and pasting (on by default; Settings > General)
- Text, rich text, links, images, files, colors and code, each with its own card; rich text keeps its formatting (from RTF, or HTML through a built-in converter that never loads remote content)
- Instant search, with close matches for typos and abbreviations ("quartely", "gthb") after exact ones, and filters: `type:link`, `app:safari`, `device:macbook` (or `device:unknown`), `after:2025-01-01`, `before:today`, `board:work`, and negation such as `-type:image`
- Text inside images is searchable (on-device OCR with Apple Vision)
- Link previews with page title, image and site icon, also searchable; only public http(s) URLs are fetched, never local or private network addresses
- Paste As (⌥Return or right-click): case changes, trimming, single line, tracking-free links, pretty or minified JSON, sorted or unique lines, URL encoding, and Markdown as formatted text; only options that change the text are shown
- Optional automatic cleanup on every paste: trim whitespace and remove link tracking parameters (both off by default)
- Snippet placeholders in pinboard items, expanded when pasting (see below)
- Rules that expire or pin new copies automatically (see below)
- Statistics window: items added per day and by time of day, types, top apps, most pasted items and devices, over 7, 30 or 90 days, for all devices or one, in light and dark mode; a Table view and Copy Data give the exact numbers for a spreadsheet
- Export to a restorable ZIP backup, JSON or CSV
- Optional read-only access for AI tools such as Claude and Cursor through a local MCP server (off by default)
- Pinboards with colors, plus a Paste Stack (⇧⌘C) for pasting several items in order, oldest or newest first
- Smart pinboards: save a search (⌘S in the shelf) as a tab that always shows what in history matches it, such as `type:link app:slack`
- Jump from a search result to its place in history or its pinboard (⌘G)
- Clear just the last 15 minutes, hour or 24 hours of history, or all of it
- Privacy: ignores password managers and concealed clipboard content, skips text that looks like API keys, private keys, tokens or card numbers (or, if you choose, keeps them hidden for a few minutes), supports custom ignore patterns, and hides the shelf from screen sharing
- Import history, pinboards, OCR text and shortcuts from Paste
- A command line interface for scripts

## Install

Download `Pasted-<version>-arm64.dmg` (Apple silicon Macs, macOS 14 or later), check it against `SHA256SUMS.txt` from the same release (`shasum -a 256 -c --ignore-missing SHA256SUMS.txt`; older releases have a `.sha256` file instead), open it and drag Pasted into Applications.

From 0.3.0 on, releases are signed with a Developer ID (MacKenzie Glanzer, team `ZD5VPNGP3T`) and notarized by Apple, so Pasted opens like any other app and the Accessibility permission carries over to new versions. Check a download with `spctl -a -vv /Applications/Pasted.app` ("Notarized Developer ID") or `codesign -dr - /Applications/Pasted.app` (`certificate leaf[subject.OU] = ZD5VPNGP3T`).

0.3.0 also changed the bundle identifier from `io.pasted.app` to `io.github.houseofmackee.pasted`. Coming from 0.2.x or earlier, macOS asks for Accessibility permission once more (the welcome window opens the right System Settings page), launch at login is registered again by itself, and an encryption key is moved to the new Keychain item automatically (macOS may ask once to allow that). 0.2.x releases were not notarized: their first launch needs **Open Anyway** in System Settings > Privacy & Security, as their "Read Me First.txt" explains.

## Build and run

Requires an Apple silicon Mac with macOS 14 or later and a Rust toolchain.

```sh
cargo run --release            # run from the terminal
scripts/bundle.sh              # build and sign dist/Pasted.app (Developer ID if present; see scripts/lib.sh)
scripts/make_dmg.sh            # build dist/Pasted-<version>-arm64.dmg and its .sha256, then mount and check it
scripts/dist.sh                # make_dmg.sh, plus a zip and SHA256SUMS.txt: everything a release uploads
scripts/make_icon.sh           # rebuild macos/AppIcon.icns after editing macos/AppIcon.svg (needs librsvg)
open dist/Pasted.app
```

Pasting directly into other apps needs Accessibility permission (System Settings > Privacy & Security > Accessibility). Grant it to `Pasted.app`; when running through `cargo run`, the permission applies to your terminal instead. Without it, choosing an item still puts it on the clipboard so you can press ⌘V yourself.

On first launch a welcome window shows the shortcut, the permission status (updated live, with a button that opens the right System Settings pane) and, when Paste is installed, the import. Open it again with **Welcome…** in the menu bar icon's menu.

VoiceOver reads each card as one button (type, source app, age, content and position; sensitive items are never read out) and announces search results and selection changes. Pasted also follows the system Reduce Motion and Increase Contrast settings.

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
| ⌘S | Save the current search as a smart pinboard |
| ⌘G | Show the selected item in history (or its pinboard), leaving the search |
| Esc | Close preview, clear search, close shelf |

Right-click a card for more actions, including pinning and Show in Clipboard or a pinboard. Drag cards into other apps.

A smart pinboard (magnifier on its tab) is a saved search over history: it holds no pinned items, and typing in it narrows its search further. Right-click its tab to edit the search, rename, recolor or delete it. Its search can start from a regular pinboard instead of history, as in `board:Work type:link`. Items cannot be pinned to a smart pinboard, and rules skip them.

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
pasted search type:link github    # same syntax as the shelf; --exact skips close matches
pasted show 42                    # print an item's text
pasted copy 42 [--plain]          # put an item back on the clipboard
pasted copy 42 --as snake         # copy with a cleanup applied
pbpaste | pasted transform clean-links   # cleanups work on stdin too (--list shows all)
echo 'Due {date+7d}' | pasted expand --set Name=Ada   # expand snippet placeholders
echo hi | pasted add              # save text
pasted pin 42 Snippets            # pin (creates the pinboard if needed)
pasted board list
pasted board create Links --search "type:link"   # a smart pinboard; `pasted board search Links QUERY` edits it
pasted clear --last 1h            # remove the last hour of history (or --all); pinboards are kept
pasted import paste --dry-run     # preview an import from Paste
pasted open                       # show the shelf of the running app
pasted settings --tab privacy     # show a Settings tab
pasted preview <url>              # fetch a link preview (debugging)
pasted export backup.zip         # also .json or .csv; --board and --query filter
pasted import archive backup.zip # restore a backup
pasted encryption [status|enable|disable]
pasted mcp                        # read-only MCP server for AI tools (see below)
pasted stats [--days 30]          # summary with a per-device breakdown; --device NAME limits it, --show opens the window,
                                  # --tsv prints every chart's values as tab-separated tables
pasted paths
```

Use `--json` with `list` and `search` for machine readable output.

## Importing from Paste

Choose **Import from Paste…** in the menu bar icon's menu, or run `pasted import paste`. The welcome window shown on first launch also offers it.

Link previews Paste already stored (titles, images, icons) come along, so those links are not fetched again.

Imports copy history as it is by default. To leave sensitive items behind, choose what to skip in Settings > Data (or the `[import]` section of the settings file): items from ignored apps, API keys and tokens, text matching your ignore patterns, and short snippets that mention a password. Each check is optional and off by default; already imported items are never removed.

The importer copies Paste's database to a temporary directory and reads the copy, so Paste's own files are never modified. Running it again only adds items that are new since the last import. History retention and shortcuts are taken from Paste's settings. Quit Paste first for the most complete import.

## Sensitive items

By default, content that password managers mark as private and text that looks like a password, API key, token or card number is never saved. If you would rather be able to paste a one-time code or temporary password again, set Settings > Privacy > Those items to be kept for 1, 5 or 15 minutes or 1 hour (`sensitive_expiry = "5m"` in the settings file). Kept items:

- show as `••••••••` with a countdown on their card; preview reveals them, pasting works normally,
- are never shared with AI tools, exported, or sent to the link preview fetcher, and rules never pin them,
- disappear from lists and search at their deadline and are deleted within seconds, with the database set to overwrite deleted content.

Excluded apps and your own ignore patterns still mean never saved.

## Encryption at rest

Turn it on in Settings > Privacy > Encryption, or with `pasted encryption enable` while the app is not running (`status` and `disable` work the same way). Pasted then:

- creates a random key in your login Keychain (item `io.github.houseofmackee.pasted`; versions before 0.3.0 used `io.pasted.app`, and the key is moved automatically),
- encrypts the history database with SQLCipher (AES-256, using macOS CommonCrypto),
- encrypts each stored image and file with AES-256-GCM under a name that does not reveal its content.

This keeps other apps, other user accounts and backups (Time Machine, synced folders) from reading your history. Know the limits: if the Keychain item is deleted the history cannot be recovered; exports and backups you make are not encrypted; FileVault already protects the disk while the Mac is off; and unencrypted copies from before you turned it on may remain in free disk space until it is reused.

## AI tools (MCP)

`pasted mcp` is a local, read-only [Model Context Protocol](https://modelcontextprotocol.io) server on stdin/stdout. It speaks MCP 2026-07-28 and, for clients that still use the `initialize` handshake, 2025-11-25, 2025-06-18 and 2025-03-26. Tools: `search_clipboard` (same filters as the shelf), `recent_clipboard`, `get_clipboard_item` and `list_pinboards`.

Access is **off by default**. Turn it on in Settings > AI, where you also choose whether clipboard history is included or only pinboards, which pinboards, which item types, and the most results per request. Images and files are never shared. Every request is appended to `mcp.log` in the data folder (Settings > AI > Open Access Log).

Settings > AI copies the setup for you. By hand:

```sh
claude mcp add pasted -- /Applications/Pasted.app/Contents/MacOS/pasted mcp
```

Claude Desktop (`claude_desktop_config.json`):

```json
{ "mcpServers": { "pasted": { "command": "/Applications/Pasted.app/Contents/MacOS/pasted", "args": ["mcp"] } } }
```

## Export and backup

`pasted export` (or Settings > Data > Export) writes:

- **`.zip`**: a full backup with every image, file and format, pinboards, OCR text and link previews. Restore it with `pasted import archive <file>` or Settings > Data > Restore Backup; items you already have are skipped, so restoring twice is harmless.
- **`.json`**: the same details without images or files, for scripts.
- **`.csv`**: one row per item for spreadsheets.

Limit an export with `--board NAME` or `--query "type:link after:2026-09-01"`. Restored history older than your retention setting is removed at the next cleanup, as with imports.

## Settings

Choose **Settings…** in the menu bar icon's menu or press ⌘, in the shelf. The window has four tabs:

- **General**: history retention, size limit, shelf search and tabs alignment, sounds, launch at login, link previews
- **Pasting**: direct pasting, plain text default, automatic cleanup, Accessibility status
- **Shortcuts**: click a field and press a new combination (global shortcuts are paused while recording)
- **Privacy**: confidential content, secret detection, screen sharing visibility, apps to ignore, ignore patterns
- **Data**: storage statistics, Paste import, clear history (recent or all), data folder

Changes apply immediately and are saved to `~/Library/Application Support/Pasted/settings.toml`. The file can also be edited by hand; Pasted notices the change within a couple of seconds.

```toml
[general]
history_retention = "month"     # day, week, month, year, forever
max_item_size_mb = 64
paste_plain_text_by_default = false
paste_directly = true
sounds = true
launch_at_login = false
link_previews = true
fuzzy_search = true             # close matches after exact ones
paste_stack_order = "oldest_first"   # or "newest_first"
shelf_bar_alignment = "left"    # shelf search and tabs: "left", "center" or "right"

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
sensitive_expiry = ""          # "" never saves sensitive items; "5m" keeps them briefly

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
cargo test                     # unit tests, pasteboard integration tests, card layout snapshot
cargo clippy --all-targets
cargo bench --bench store      # capture and search benchmarks (search budget: 20 ms at 100k items; currently 0.1 to 10 ms)
```

The pasteboard tests use private, uniquely named pasteboards and never touch your clipboard. The card layout test (`tests/card_layout.rs`) builds a real card for each kind of item and compares every subview's frame and text with `tests/snapshots/card_layout.txt`; after an intended change, review the diff and run `UPDATE_SNAPSHOTS=1 cargo test --test card_layout`. Before a release, work through [docs/RELEASE_CHECKLIST.md](docs/RELEASE_CHECKLIST.md).

GitHub Actions runs `cargo fmt --check`, `cargo clippy -D warnings` (debug and release), `cargo test`, a bundle build and `cargo deny check` (see `deny.toml`) on every push to master and every pull request.

### Releasing

Releases are built and published by GitHub Actions, not from a local machine. Bump the version in `Cargo.toml` (and `CFBundleVersion` in `macos/Info.plist`), commit, then push a matching tag:

```sh
git tag v0.3.0 && git push origin master v0.3.0
```

`.github/workflows/release.yml` checks the tag matches `Cargo.toml`, signs with the Developer ID from the repository secrets `DEVELOPER_ID_P12_BASE64` and `DEVELOPER_ID_P12_PASSWORD`, notarizes the app and the DMG with the App Store Connect API key (`NOTARY_API_KEY_BASE64` secret; `APPLE_TEAM_ID`, `NOTARY_KEY_ID` and `NOTARY_ISSUER_ID` variables), refuses to publish anything Gatekeeper would block, and publishes the DMG, zip and `SHA256SUMS.txt`. Locally, `scripts/dist.sh` does the same when the Developer ID is in the keychain and `PASTED_NOTARY_KEY`, `PASTED_NOTARY_KEY_ID` and `PASTED_NOTARY_ISSUER` are set. Every release is published as a pre-release; mark it as a regular release on GitHub once the manual checks in [docs/RELEASE_CHECKLIST.md](docs/RELEASE_CHECKLIST.md) are done. Do not create releases by hand with `gh release create`.

See [PLAN.md](PLAN.md) for the roadmap.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
