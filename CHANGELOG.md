# Changelog

All notable changes to Pasted. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The section for a version is what
the update window and the GitHub release show.

## [Unreleased]

### Changed

- The update window shows what changed in the new version, taken from this
  changelog, instead of only a link to the release notes.

## [0.3.2] - 2026-10-12

### Added

- An option to refuse pinning sensitive items: Settings > Privacy, "Don't
  allow pinning kept items" (off by default). Pinning keeps an item for
  good, so with this on a kept password or code always disappears at its
  deadline. `pasted pin` respects it too; items pinned before stay.

## [0.3.1] - 2026-10-12

### Fixed

- The hint in the shelf's search field is readable again: it is darker in
  light mode, brighter in dark mode, and the field no longer lets dark
  windows behind the shelf show through.

## [0.3.0] - 2026-10-12

### Added

- Pasted updates itself: Check for Updates… in the menu bar icon's menu,
  and automatic checks in Settings > General. Updates are only offered
  once a release is marked as a regular release.
- Smart pinboards: save a search (Cmd-S in the shelf) as a tab that always
  shows what in history matches it, such as `type:link app:slack`.
- Jump from a search result to its place in history or its pinboard
  (Cmd-G, or Show in Clipboard on the card's menu).
- Clear just the last 15 minutes, hour or 24 hours of history.
- A `device:` search filter, for example `device:macbook`.

### Changed

- Releases are signed with a Developer ID and notarized by Apple, so
  Pasted opens like any other app.
- The bundle identifier is now `io.github.houseofmackee.pasted`. Coming
  from 0.2.x, macOS asks for Accessibility permission once more; an
  encryption key moves to the new Keychain item by itself.
- Faster search on large histories (every search under 10 ms at 100,000
  items).

## [0.2.2] - 2026-10-11

### Fixed

- The "close matches" checkbox in Settings > General is no longer cut off.

### Changed

- Releases are built and published by GitHub Actions.
- Updated the encryption and other libraries; encrypted history stays
  readable.

## [0.2.1] - 2026-10-10

### Changed

- Releases are signed with a stable certificate, so the Accessibility
  permission carries over to new versions.

## [0.2.0] - 2026-10-10

### Added

- Click sounds when copying and pasting, also for your own Cmd-V.
- Click the menu bar icon to open the shelf; right-click it for the menu.
- The shelf's search field and tabs can be left aligned, centered or right
  aligned.

## [0.1.0] - 2026-10-10

First release: clipboard history on a visual shelf, pinboards, fast search
with filters and close matches, text in images (OCR), link previews, rich
text, Paste As cleanups, snippet placeholders, rules, a Paste Stack,
statistics, export and backups, optional encryption at rest, a read-only
MCP server for AI tools, VoiceOver support, and importing from Paste.

[Unreleased]: https://github.com/houseofmackee/pasted/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/houseofmackee/pasted/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/houseofmackee/pasted/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/houseofmackee/pasted/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/houseofmackee/pasted/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/houseofmackee/pasted/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/houseofmackee/pasted/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/houseofmackee/pasted/releases/tag/v0.1.0
