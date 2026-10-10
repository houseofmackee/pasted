# Pasted

A fast, private clipboard manager for macOS, written in Rust. Inspired by [Paste](https://pasteapp.io), with history import from an existing Paste installation.

Everything stays on your Mac: no accounts, no telemetry, no network access.

## Features

- Unlimited clipboard history with a visual shelf at the bottom of the screen (default shortcut ⇧⌘V)
- Text, rich text, links, images, files, colors and code, each with its own card
- Instant search with filters: `type:link`, `app:safari`, `after:2025-01-01`, `before:today`, `board:work`, and negation such as `-type:image`
- Text inside images is searchable (on-device OCR with Apple Vision)
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

## Command line

```sh
pasted list -n 20                 # recent history
pasted search type:link github    # same syntax as the shelf
pasted show 42                    # print an item's text
pasted copy 42 [--plain]          # put an item back on the clipboard
echo hi | pasted add              # save text
pasted pin 42 Snippets            # pin (creates the pinboard if needed)
pasted board list
pasted import paste --dry-run     # preview an import from Paste
pasted open                       # show the shelf of the running app
pasted settings --tab privacy     # show a Settings tab
pasted stats
pasted paths
```

Use `--json` with `list` and `search` for machine readable output.

## Importing from Paste

Choose **Import from Paste…** in the menu bar menu, or run `pasted import paste`. On first launch with an empty history, Pasted offers the import automatically.

The importer copies Paste's database to a temporary directory and reads the copy, so Paste's own files are never modified. Running it again only adds items that are new since the last import. History retention and shortcuts are taken from Paste's settings. Quit Paste first for the most complete import.

## Settings

Choose **Settings…** in the menu bar menu or press ⌘, in the shelf. The window has four tabs:

- **General**: history retention, size limit, direct pasting, plain text default, sounds, launch at login, Accessibility status
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
