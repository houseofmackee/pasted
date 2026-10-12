# Pasted: implementation plan

Pasted is a native macOS clipboard manager written in Rust, modeled on [Paste](https://pasteapp.io) (Wiheads). It aims for feature parity with Paste's core experience, a few improvements, and a one-click importer for an existing Paste installation's history, pinboards and settings.

## 1. Goals and non-goals

**Goals**

- Unlimited, searchable clipboard history that survives restarts.
- A keyboard-first, visual "shelf" UI summoned by a global hotkey.
- Pinboards (named, colored collections) and a Paste Stack for sequential pasting.
- Strong privacy defaults: ignore passwords and sensitive apps, everything local by default.
- Import of history, pinboards and settings from the installed Paste app.
- Small, fast, low idle CPU (target under 0.5% idle, under 60 MB RSS with 10k items).

**Non-goals for v1**

- iOS/iPadOS companion apps.
- Team/shared pinboards (needs a server or CloudKit sharing; revisit after sync).
- Mac App Store distribution (sandboxing blocks synthetic paste via accessibility). Distribution is a direct-download DMG only.
- Intel Macs. Builds target Apple silicon (arm64) only.

## 2. Technology choices

| Concern | Choice | Reason |
| --- | --- | --- |
| UI | Native AppKit via `objc2`, `objc2-app-kit`, `objc2-foundation` | Paste's UX depends on `NSPanel` behavior (non-activating floating panel, all Spaces, over full-screen apps). Native gives this directly and keeps memory low. |
| Pasteboard | `NSPasteboard` through `objc2-app-kit` | Need every UTI representation, `changeCount`, and the `org.nspasteboard.*` privacy markers. |
| Global hotkeys | Carbon `RegisterEventHotKey` (via small FFI) | Works without accessibility permission; same mechanism Paste uses. |
| Synthetic paste | `CGEventPost` Cmd+V via `objc2-core-graphics` | Requires Accessibility permission (same as Paste). |
| Storage | SQLite via `rusqlite` (bundled, FTS5 enabled) | Single file, WAL mode, full text search. |
| Large blobs | Content-addressed files under `~/Library/Application Support/Pasted/blobs/` | Keeps DB small; dedupe by SHA-256. |
| OCR | Apple Vision (`VNRecognizeTextRequest`) via `objc2-vision` | On-device, matches Paste's image text search. |
| Settings | `serde` + TOML file, mirrored to an in-memory struct | Human-editable, easy to export/import. |
| Import decoding | `plist`, `flate2`, `lzfse` crates | Paste stores data as JSON, deflate, LZFSE and binary plists (see section 6). |
| Logging | `tracing` + `tracing-oslog` | Shows up in Console.app. |
| Launch at login | `SMAppService` (macOS 13+) | Modern, no helper app. |
| Updates | None for now (Sparkle 2 deferred) | Users download new DMGs by hand until updates are revisited. |
| Packaging | `scripts/bundle.sh` assembles and signs `Pasted.app`; `scripts/dist.sh` notarizes and builds the DMG and zip for direct download | Reproducible from CLI and GitHub Actions. |

Minimum macOS: 14.0 (Sonoma). Set in `.cargo/config.toml` and `macos/Info.plist`.

## 3. Architecture

```
src/
  main.rs              app entry, NSApplication setup, LSUIElement agent app
  app/                 AppDelegate, menu bar status item, lifecycle
  clipboard/
    monitor.rs         polls NSPasteboard.changeCount (250 ms, adaptive)
    capture.rs         reads all representations, builds ClipItem
    classify.rs        text / rich text / link / image / file / color / code detection
    privacy.rs         concealed/transient markers, excluded apps, regex rules
    writer.rs          writes an item back to NSPasteboard (full or plain text)
  paste/
    inject.rs          CGEvent Cmd+V into the frontmost app
    stack.rs           Paste Stack queue
  store/
    db.rs              rusqlite pool, migrations
    schema.sql         items, representations, pinboards, apps, fts tables
    blobs.rs           content-addressed blob files
    retention.rs       history pruning by age / count / size
  search/
    query.rs           query parser (text + filters like type:, app:, before:)
    fts.rs             FTS5 queries, ranking
    ocr.rs             Vision OCR worker queue
  ui/
    shelf.rs           bottom panel (NSPanel), horizontal card strip
    card.rs            card rendering per item type
    preview.rs         large preview / Quick Look
    editor.rs          edit text items in place
    pinboards.rs       pinboard tabs, create/rename/color
    settings/          preferences window (General, Shortcuts, Rules, Sync, Import, Advanced)
    welcome.rs         first-launch walkthrough (shortcut, permission, import)
    a11y.rs            VoiceOver descriptions, announcements, display preferences
  hotkeys.rs           Carbon hotkey registration, shortcut model
  settings.rs          Settings struct, load/save, change notifications
  import/
    paste_app.rs       importer for the Paste app (section 6)
    decode.rs          blob decoding (deflate, LZFSE, bplist, external refs)
  mcp/                 optional local MCP server (phase 5)
xtask/                 bundling, signing, notarization, DMG
macos/                 Info.plist, entitlements, icons
```

Threading: the main thread owns AppKit. Clipboard capture runs on the main run loop timer (pasteboard access is cheap) and hands heavy work (hashing, thumbnailing, OCR, DB writes) to a worker thread pool over channels. The UI reads from the DB through a small query cache.

### Data model (SQLite)

- `items(id, kind, title, created_at, last_used_at, use_count, source_app_id, device_id, checksum, preview_text, size_bytes, pinned)`
- `representations(item_id, uti, blob_hash | inline_data, size)`: every pasteboard type preserved so pasting back is lossless.
- `pinboards(id, name, color, sort_index, created_at)` and `pinboard_items(pinboard_id, item_id, sort_index)`.
- `apps(id, bundle_id, name, icon_png)`.
- `items_fts` (FTS5 over title, text, OCR text, URL, app name).
- `ocr(item_id, text, confidence)`.
- `import_runs(id, source, started_at, finished_at, items_imported, items_skipped)` for idempotent re-import.

## 4. Feature list

### 4.1 Parity with Paste

1. **Clipboard history**: capture everything copied, unlimited by default, dedupe identical consecutive copies (by checksum, bump timestamp instead of duplicating).
2. **Content types**: plain text, rich text (RTF/HTML), links (with page title and favicon/OG image fetched lazily), images, files/folders, colors (hex/rgb detection with swatch), code (language detection, monospace preview).
3. **Shelf UI**: bottom-of-screen panel with horizontal cards, source app icon/color, relative timestamp, character count or image dimensions. Opens on the active screen, over full-screen apps, without stealing focus from the target app.
4. **Keyboard navigation**: arrows, Return to paste, Shift+Return to paste as plain text, Cmd+1 to Cmd+9 quick paste, Space for preview, Cmd+F search, Cmd+E edit, Cmd+Delete delete, Tab to switch pinboards, Esc to close.
5. **Search and filters**: instant full text search, filter by type, source app, date, and pinboard; OCR text from images is searchable.
6. **Pinboards**: create, rename, color, reorder; drag items in; per-pinboard shortcuts (next/previous pinboard).
7. **Paste Stack**: dedicated hotkey opens a stack; copy several items, then paste them in order (FIFO or LIFO option).
8. **Edit and rename**: edit text items, set custom titles.
9. **Drag and drop**: drag cards out into any app; drag files/text into the shelf to save them.
10. **Preview**: large preview and Quick Look for files and images.
11. **Paste as plain text** (per paste and as a global default).
12. **Privacy rules**: ignore `org.nspasteboard.ConcealedType`, `TransientType`, `AutoGeneratedType`; excluded app list (prefilled with password managers); hide content during screen sharing (Paste's `isScreenSharingContentVisible`).
13. **History retention**: day, week, month, year, forever; plus optional size cap.
14. **Settings**: launch at login, sounds on copy/paste, shelf position, theme, shortcuts.
15. **Menu bar item** with quick access, pause capture, and preferences.
16. **Sync** across Macs via the user's private iCloud (CloudKit). Planned for a later phase since it needs an Apple Developer team and entitlements.
17. **AI integration (MCP)**: local MCP server that lets tools like Claude query history, with an explicit per-pinboard / per-type allowlist. Paste ships this as `PastePackages_PasteMCP`.

### 4.2 Improvements over Paste

- **Fully local and open data**: documented SQLite schema, plain TOML settings, `pasted export` to JSON/ZIP. No telemetry (Paste embeds PostHog, Sentry, Firebase Crashlytics).
- **CLI companion**: `pasted list`, `pasted search <q>`, `pasted copy <id>`, `pasted pin <id> <board>`, piping from stdin (`echo hi | pasted add`). Handy in scripts and terminals.
- **Rule engine**: regex and app-based rules, for example "never save strings that look like API keys / JWTs / credit cards", "auto-expire items from Terminal after 1 hour", "auto-pin items matching X to board Y".
- **Text transforms on paste**: trim whitespace, change case, strip formatting, URL clean-up (remove tracking params), JSON pretty-print, Markdown to rich text.
- **Snippets with placeholders**: pinboard items can contain `{date}`, `{clipboard}`, `{cursor}` style variables expanded at paste time.
- **Encrypted at rest option**: SQLCipher plus a Keychain-stored key for users who want it.
- **Sensitive item auto-expiry**: items flagged as secrets are kept for N minutes then purged.
- **Fast fuzzy search** mode in addition to FTS (good for short queries).
- **Per-device and per-app statistics** screen (most pasted, most copied from).
- **Accessibility**: full VoiceOver labels on cards, reduced motion support. (Done.)

## 5. Phased roadmap

**Phase 0: foundation (done)**
Cargo project, git repo, macOS-only build guard, Info.plist template, deployment target.

**Phase 1: headless core**
- `store/` with migrations, blobs, retention; unit tests against a temp DB.
- `clipboard/monitor` + `capture` + `classify` + `privacy`, running as a menu bar agent app with no shelf yet.
- CLI subcommands (`list`, `search`, `copy`) to exercise the core before UI exists.
- Exit criteria: copies from Safari, Finder, Preview, Terminal and 1Password are captured or correctly ignored; restart keeps history.

**Phase 2: shelf UI and pasting**
- `NSPanel` shelf with virtualized card strip (only visible cards are realized).
- Keyboard navigation, quick paste, plain text paste, Accessibility permission onboarding, `CGEvent` paste.
- Global hotkey (default Shift+Cmd+V, matching Paste).
- Exit criteria: summon, pick and paste into the previously focused app in under 100 ms end to end.

**Phase 3: Paste importer** (section 6). Placed early so the app is immediately useful with existing history.

**Phase 4: organization and search**
- Pinboards, Paste Stack, edit/rename, drag and drop, preview, filters, OCR worker, link metadata fetch.
- Settings window with all tabs.

**Phase 5: improvements and integrations**
- Rule engine, transforms, snippets, encryption, statistics, MCP server, export.

**Phase 6: distribution**
- Bundle script (arm64 build, assemble `.app`, copy `Info.plist`, icons), codesign with a Developer ID, notarization and stapling of the app and DMG, DMG and zip for direct download. Sparkle is deferred (section 9).
- Launch at login via `SMAppService`.

**Phase 7: sync**
- CloudKit private database sync (requires Apple Developer account and entitlements). Conflict-free design: items immutable, pinboard membership as last-writer-wins records.

## 5a. Progress (2026-10-10)

Done:

- **Phase 0 and 1**: storage (SQLite with trigram FTS5, content-addressed blobs, retention, garbage collection), capture, classification, privacy filter, settings, CLI.
- **Phase 2**: menu bar agent app, non-activating shelf panel with lazily built cards, global hotkeys (Carbon), keyboard navigation, quick paste, plain text paste, Cmd+V injection, drag out, context menus, preview, edit and rename dialogs. Measured shelf open time: 17 to 27 ms warm, 94 ms cold (release build, 150 item page).
- **Phase 3**: Paste importer (history, pinboards, OCR text, shortcuts, retention), idempotent, verified against a real Paste 6.6.7 store: 1432 of 1432 items decoded.
- **Phase 4 (partial)**: pinboards (create, rename, recolor, delete, pin/unpin), Paste Stack, Vision OCR worker, search filters.
- **Phase 4 Settings window**: General, Shortcuts (recorder fields), Privacy (excluded apps list, validated ignore patterns), Data (stats, import, clear history). Changes apply immediately; hand edits to settings.toml are picked up automatically.
- **Link previews**: background fetcher (public http(s) only, private and local hosts rejected at every redirect, size and time limits, opt-out setting), previews imported from Paste, shown on cards and in the preview panel, and indexed for search.
- **Rich text rendering**: RTF through AppKit, HTML through a small built-in converter (no WebKit, no remote content); fonts scaled to the card, background highlights dropped, low-contrast source colors replaced. Preview panel made opaque for readable text.
- **Optional import privacy filter**: four independent checks (ignored apps, secrets, ignore patterns, password mentions), all off by default.
- **Text cleanup on paste**: Paste As menu (17 transforms, only those that change the text are offered), optional automatic trimming and link tracking removal, Pasting settings tab, `pasted copy --as` and `pasted transform`.
- **Snippet placeholders**: dates with offsets and formats, clipboard, uuid, cursor positioning, input fields with one dialog; pinboard items only, unknown braces untouched, escape with a backslash, Paste As > Without Placeholders, `pasted expand`.
- **Rules**: conditions on app, item type and text pattern; actions expire after a duration or pin to a pinboard (pinned items never expire); Settings > Rules with an editor, invalid rules flagged, `pasted rules` and `pasted rules test`.
- **Statistics window**: stat tiles, items added per day and by hour, types, top apps (tail folded into Other), most pasted; 7/30/90 day ranges; reference data-viz palette validated for both modes; tooltips and VoiceOver descriptions; `pasted stats --show`.
- **Export and restore**: ZIP backup (manifest plus content-addressed data files, verified by hash on restore, size-limited entries), JSON and CSV, `--board` and `--query` filters, Settings > Data Export and Restore Backup running in the background; restore keeps duplicates within a backup and skips content already present.
- **Encryption at rest** (opt-in): SQLCipher via CommonCrypto (no OpenSSL), AES-256-GCM sealed blob files with keyed names, master key in the Keychain with derived subkeys, encryption detected from the files, conversion builds a copy and swaps it in, workers gated during the switch; Settings > Privacy and `pasted encryption`.
- **MCP server**: `pasted mcp`, dual-era (2026-07-28 stateless with `server/discover`, plus `initialize` for 2025-11-25 and earlier), read-only tools with `readOnlyHint`, structured output, access off by default and limited by history/pinboard/type settings, per-request access log, Settings > AI with setup helpers.
- **Sensitive item auto-expiry** (opt-in): concealed and secret-looking copies kept for a chosen time instead of skipped; masked on cards (length hidden too), excluded from MCP, export, link previews and rule pinning; hidden from search at the deadline and swept within 5 s; SQLite `secure_delete` plus a WAL checkpoint so no copy remains in database files.
- **Fuzzy search**: after exact (FTS/LIKE) results run out, close matches fill the page: fzf-style subsequences within a single token (nucleo-matcher) and 1 to 2 typos per word; filters apply; most recent 5,000 candidates; about 10 ms on 1,432 real items; "≈ Close match" on cards; in the shelf, `pasted search` (`--exact` to skip) and MCP; setting in General.
- **Per-device statistics**: device filter in the Statistics window (every tile and chart), a Devices panel with item share, top app and last copy, `pasted stats --device` and a per-device summary; window sized to the screen and scrolls only when needed.
- **Paste Stack order**: oldest first (default) or newest first, fixed per stack; queue logic in a tested module; panel shows the order and is now opaque for readable text.
- **Accessibility**: welcome window on first launch and from the menu (shortcut, live Accessibility status with a deep link, Paste import; replaces the automatic import prompt), also shown the first time a paste needs permission; cards are single VoiceOver buttons with a spoken description (type, title, app, age, content, expiry, position; sensitive items never read out) and selection, results and panels are announced; labelled search field and tabs; Reduce Motion turns off window animations; Increase Contrast adds card borders.
- **Statistics table**: Charts or Table view in the Statistics window (every chart's exact values in selectable, aligned tables; all apps rather than the top six), Copy Data puts tab-separated text and HTML tables on the clipboard for spreadsheets and documents (not added to history), `pasted stats --tsv`; sensitive items stay hidden.
- **App icon**: stacked shelf cards on a blue-violet tile, drawn on the macOS icon grid; SVG source with `scripts/make_icon.sh` to rebuild the `.icns`; used by `Pasted.app` and built into the binary so dialogs show it when running from a terminal.
- **Testing**: the crate is now a library plus a thin binary, so tests and benchmarks can reach the code. Pasteboard integration tests on private pasteboards (plain, rich, multi-item, image, concealed, patterns, size limit, store and write back exactly, duplicate copies); card layout snapshot test on the main thread (12 kinds, every subview's frame and text, checks that nothing spills outside the card and sensitive text never shows); criterion benchmarks (one insert into 10k history about 0.37 ms, 10k batch import about 1 s, all 7 shelf searches at 100k items within 20 ms, 0.2 to 18.6 ms); `docs/RELEASE_CHECKLIST.md` for full-screen apps, displays, Spaces, Stage Manager, secure input, install and accessibility.
- **One plain text encoding**: the UTF-16 copy of the plain text that macOS adds to rich text copies is dropped when it repeats the UTF-8 text (kept when it differs, for example bullets some apps only put in the UTF-16 version), on capture and on every insert; checksums leave it out so old and new copies are still duplicates; migration 5 cleans stored items (45 of 1,432 imported items, 6 kept because their UTF-16 text differs). Pasting still offers UTF-16, which macOS recreates.
- **Search headroom**: the full text match runs once and decides the plan: up to 2,000 matches are looked up directly, more walk a new compact index (`items_recent`, migration 6) newest first and stop after a page; fuzzy fill loads only id, time and text for its 5,000 candidates, scores them on up to 4 cores, skips impossible typo comparisons without allocating, and loads full items for the winners only; the duplicate check is pinned to its index. At 100k items every shelf search takes 0.1 to 9.6 ms (common word 18.6 to 5.8 ms, two words 17.9 to 9.5 ms, app filter 17.6 to 4.5 ms, fuzzy fill 14.1 to 4.6 ms); capture speed unchanged. Tests check that every plan returns the same results as the single query and that the planned indexes are used.
- **Click sounds and menu bar clicks**: original copy and paste clicks synthesized in code (no sound files, nothing taken from Paste), on by default for new installs, also for Paste Stack pastes; left-click on the menu bar icon toggles the shelf, right-click or Control-click opens the menu (popped up under the icon in the system appearance), VoiceOver press opens the shelf and "show menu" (VO-Shift-M) opens the menu; your own Cmd+V in any app also clicks (a listener for Cmd+V only, while sounds are on and Accessibility is allowed; Pasted's own Cmd+V is not counted twice). Shelf search and tabs can be left aligned, centered or right aligned (Settings > General); the item count moves to the left edge when the bar is on the right.
- **Smart pinboards, Jump to item, Clear recent history, device filter** (from the 2026-10-12 comparison with Paste, Pastebot, Raycast and others): smart pinboards are saved searches stored on the pinboard (`query`, migration 7), shown as tabs with a tinted magnifier; typed and saved filters must both match; they can build on a regular pinboard (`board:Work type:link`) but never on another smart one; nothing can be pinned to them, rules skip them, backups keep them, the MCP server only offers them when history is shared and never doubles items; created with ⌘S or + New while searching, or `pasted board create NAME --search QUERY`. ⌘G and Show in Clipboard / Show in ‹pinboard› leave a search for the item's place, loading pages until it is found. Clear History has Last 15 Minutes, Last Hour, Last 24 Hours and All History (menu bar, Settings > Data, `pasted clear --last 1h|--all`), always keeping pinboards. `device:NAME` and `device:unknown` filter search everywhere. Tested in the store, the MCP server, backups and an in-process shelf test.
- **Optional refusal to pin sensitive items** (`privacy.refuse_pinning_sensitive`, off by default; Settings > Privacy "Don't allow pinning kept items"): pinning keeps an item indefinitely and clears its expiry, so a user can rule that out for sensitive items. Enforced for the card's Pin to (disabled, with a tooltip), New Pinboard from a card and `pasted pin`; items pinned before stay. Pinned items are otherwise never deleted by the history time limit, rule or sensitive expiry, or Clear History; the time limit only takes old pinned items out of clipboard history.
- **Phase 6**: `scripts/bundle.sh` builds and signs `dist/Pasted.app`; launch at login through `SMAppService`; `scripts/make_dmg.sh` builds `dist/Pasted-<version>-arm64.dmg` for direct download (built-in `hdiutil` only, LZMA compressed, app plus Applications link plus "Read Me First.txt" on opening an unnotarized app), refuses non-arm64 binaries, mounts the result to verify the signature, architecture and layout, and writes a SHA-256 file. Notarization and Sparkle came in 0.3.0 (section 9).

Next:

Candidates from the 2026-10-12 comparison (not scheduled):
- User-defined Paste As scripts (shell filters with preview and shortcuts, like Pastebot).
- Merge items: select several cards and paste them joined.
- Restore the previous clipboard after pasting from the shelf (clipboard undo).
- Snippet keywords typed in any app (opt-in; needs reading keystrokes).
- Shortcuts app actions (needs Swift App Intents).
- Sync between Macs (see Phase 7).

Known limitations:

- Paste's OCR index on this Mac only references items that no longer exist, so 0 OCR texts matched on import; Pasted's own OCR worker covers the imported images instead.
- Importing with a short retention setting removes older imported history at the next cleanup; the import summary warns about this.

## 6. Importing from Paste

Findings from inspecting the installed copy on this Mac (Paste 6.6.7, Setapp edition, bundle id `com.wiheads.paste-setapp`). The importer must also handle the direct-download edition (`com.wiheads.paste`) and App Store edition, whose paths differ only by bundle id or container.

### 6.1 Locations

| Data | Path (Setapp edition) |
| --- | --- |
| Main Core Data store | `~/Library/Application Support/com.wiheads.paste-setapp/db.sqlite` (+ `-wal`, `-shm`) |
| Externally stored blobs | `~/Library/Application Support/com.wiheads.paste-setapp/.db_SUPPORT/_EXTERNAL_DATA/<UUID>` |
| Search index (FTS5, OCR) | `~/Library/Application Support/com.wiheads.paste-setapp/index.sqlite` |
| Backups | `~/Library/Application Support/com.wiheads.paste-setapp/backup/<timestamp>/` |
| Preferences | `~/Library/Preferences/com.wiheads.paste-setapp.plist` |

Candidate roots to probe, in order: `com.wiheads.paste-setapp`, `com.wiheads.paste`, and `~/Library/Containers/com.wiheads.paste*/Data/Library/Application Support/...` for sandboxed builds. Let the user pick a backup folder manually as a fallback.

### 6.2 Schema (Core Data, `Z*` tables)

- `ZITEMENTITY`: one row per clip. Key columns: `ZIDENTIFIER` (UUID), `ZRAWTYPE`, `ZTITLE`, `ZTIMESTAMP`, `ZCREATEDAT`, `ZCHECKSUM`, `ZLIST` (FK to list), `ZDISPLAYORDERINPINBOARD`, `ZSOURCEAPPLICATION`, `ZDEVICE`, `ZRAWPREVIEW`.
- `ZITEMDATAENTITY`: `ZITEM` (FK), `ZRAWPASTEBOARDITEMS` (full pasteboard contents).
- `ZLISTENTITY`: lists. `ZRAWTYPE` 1 = clipboard history, 2 = pinboard. `ZNAME`, `ZRAWATTRIBUTES` JSON (`{"type":"pinboard","colorCode":<ARGB int>}` or `{"type":"clipboard","capacity":<n>, likely days}`).
- `ZLISTMETADATAENTITY`: pinboard ordering (`ZINDEX`).
- `ZAPPLICATIONENTITY`: `ZBUNDLEIDENTIFIER`, `ZNAME`, `ZRAWCOLOR`, `ZRAWICON`.
- `ZDEVICEENTITY`: device name and type.
- Timestamps are Core Data reference dates: seconds since 2001-01-01 UTC (add 978307200 for Unix time).
- `ZRAWTYPE` for items (observed counts suggest): 0 = color, 1 = image, 3 = file, 4 = link, 5 = text. Confirm each mapping against decoded content during implementation.

### 6.3 Blob encodings

`ZRAWPASTEBOARDITEMS` starts with a one byte Core Data marker:

- `0x02` followed by a NUL terminated UUID: external storage, read `.db_SUPPORT/_EXTERNAL_DATA/<UUID>`.
- `0x01` followed by inline data, which is one of:
  - raw deflate of JSON (older items),
  - `bvx2` / `bvxn` magic: LZFSE compressed,
  - `bplist00`: binary plist.

After decompression the payload (JSON or plist) is an array of pasteboard items, each `{ "types": [...], "dataByType": { "<UTI>": <base64 or data> } }`. These map one to one onto our `representations` table, so imports are lossless.

`ZRAWPREVIEW` is JSON for older items (`text`, `textLength`, `url`, `urlName`, `urlIconData`, `urlImageData`) and a binary plist for newer ones. Use it only for titles and link metadata; regenerate thumbnails ourselves.

`index.sqlite` holds `items(id, app, timestamp, type, title, content, device)` plus `items_ocr(item_id, metadata)`. Import OCR text from here to avoid re-running Vision on every image.

### 6.4 Settings mapping

Read the preferences plist with the `plist` crate (never via `defaults write`). Shortcut values are JSON blobs such as `{"keyCode":9,"internalModifiers":...}`.

| Paste key | Pasted setting |
| --- | --- |
| `activationShortcut` | Open shelf hotkey |
| `pasteStackShortcut` | Paste Stack hotkey |
| `nextPinboardShortcut`, `previousPinboardShortcut` | Pinboard navigation |
| Clipboard list `capacity` attribute | History retention |
| `isScreenSharingContentVisible` | Hide content while screen sharing |
| `selectedListIdentifier` | Initially selected board |
| `NSWindow Frame ...` | ignored |
| licensing, PostHog, Firebase, CloudKit keys | ignored, never copied |

Excluded apps and rules live in the Core Data store or preferences depending on version; detect and map them when present, otherwise log as "not found".

### 6.5 Import procedure

1. Detect installations and show item/pinboard counts in Settings > Import (also `pasted import paste [--dry-run]`).
2. Warn if Paste is running; offer to quit it (WAL contents are only consistent while closed or when copied together).
3. Copy `db.sqlite`, `-wal`, `-shm` and `index.sqlite*` to a temp dir and open read-only. Never write to Paste's files.
4. Import apps, devices, lists (pinboards), then items in timestamp order, decoding blobs and writing representations to our blob store.
5. Idempotency: key imported items by Paste's `ZIDENTIFIER` (stored in an `external_id` column) so re-running the import only adds new items.
6. Map settings, show a summary (imported, skipped, failed with reasons), delete temp copies.
7. Tests: build a fixture Core Data DB with synthetic items covering each blob encoding; never commit real user data.

## 7. Permissions and entitlements

- Accessibility (for synthetic Cmd+V): onboarding explains why, deep links to System Settings.
- Full Disk Access is not required for importing from `~/Library/Application Support/com.wiheads.paste*`, but sandboxed Paste containers may require it; detect and prompt.
- Hardened runtime, no sandbox, Developer ID signing.

## 8. Testing strategy

- Unit tests for classify, privacy rules, query parser, retention, import decoders.
- Integration tests that drive a private named `NSPasteboard` (not the general one) to test capture and write-back.
- Snapshot tests for card layout metrics.
- Performance benchmarks (`criterion`): insert 10k items, FTS search latency under 20 ms at 100k items.
- Manual QA checklist per release: full-screen apps, multiple displays, Spaces, Stage Manager, secure input fields.

## 9. Decisions and deferred items

- **License**: open source, dual licensed MIT OR Apache-2.0 (Rust ecosystem convention). Not a paid product.
- **Bundle identifier: `io.github.houseofmackee.pasted`** (0.3.0, matching CleanShotZ), replacing the placeholder `io.pasted.app`. The encryption key moves to the new Keychain item on first use; launch at login re-registers itself; users grant Accessibility once more (together with the signing change below).
- **Distribution: direct-download DMG, Apple silicon only.** No universal binary, no Mac App Store.
- **Signing: Developer ID, notarized** (0.3.0). Releases are signed with "Developer ID Application: MacKenzie Glanzer (ZD5VPNGP3T)", the same team as CleanShotZ, with the hardened runtime and a secure timestamp; the app and the DMG are notarized with an App Store Connect API key and stapled. The designated requirement is the bundle id plus the team, so Accessibility permission survives updates. Secrets: `DEVELOPER_ID_P12_BASE64`, `DEVELOPER_ID_P12_PASSWORD`, `NOTARY_API_KEY_BASE64`; variables `APPLE_TEAM_ID`, `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID`. 0.2.1 to 0.2.2 used the self-signed "Pasted Release Signing" certificate, now only a fallback for local builds without a Developer ID (`scripts/lib.sh`).
- **Releases come from GitHub Actions** (2026-10-11, aligned with CleanShotZ). CI (`.github/workflows/ci.yml`) runs fmt, clippy in debug and release, tests, a bundle build and `cargo deny` on master and pull requests. Pushing a `v*` tag runs `.github/workflows/release.yml`, which checks the tag against `Cargo.toml`, signs, runs `scripts/dist.sh` and publishes the DMG, zip and `SHA256SUMS.txt` as a pre-release; it becomes a regular release once the manual checks in `docs/RELEASE_CHECKLIST.md` are done.
- **Updates: Sparkle** (0.3.0). Sparkle 2.10.0, pinned by checksum, embedded without its XPC services and loaded at runtime (only Developer ID builds can load it under library validation; other builds have no updater). Feed: `https://github.com/houseofmackee/pasted/releases/latest/download/appcast.xml`, so only regular (not pre-release) releases are offered; feed and archive must be signed (`SURequireSignedFeed`, `SUVerifyUpdateBeforeExtraction`) with Pasted's own EdDSA key (public `n6XLlnXBIFSevX9LiatgCD1LKe7uEcg2+x4DRQkbO2U=`; private key in the login Keychain, account `pasted`, and the `SPARKLE_PRIVATE_KEY` secret; keep a backup, losing it means no more updates). Gentle reminders for a menu bar app; Check for Updates… in the menu; automatic checks in Settings > General. Tested end to end with a signed test feed: 0.3.0 found, downloaded, verified and installed a 0.3.1.
- **Performance: no further tuning planned.** Current speed is good for day-to-day use (searches 0.1 to 10 ms and the statistics report about 0.55 s, both at 100k items).
- **Deferred: iCloud sync.** Phase 7 is on hold. Revisit later, including whether a folder-based sync (iCloud Drive/Dropbox) should come before CloudKit.
