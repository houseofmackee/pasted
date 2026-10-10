# Release checklist

Work through this for every release, on an Apple silicon Mac. Copy it into the release issue and tick items as you go. Anything that fails blocks the release or gets a written exception in the issue.

## 1. Automated checks

- [ ] `cargo fmt --check` and `cargo clippy --all-targets` are clean.
- [ ] `cargo test` passes, including the pasteboard integration tests and the card layout snapshot (`tests/card_layout.rs`). If the snapshot changed on purpose, review the diff and run `UPDATE_SNAPSHOTS=1 cargo test --test card_layout`.
- [ ] `cargo bench --bench store`: every search line says `ok` (20 ms budget at 100,000 items). Note the numbers in the release issue to spot regressions.
- [ ] Version bumped in `Cargo.toml`; `scripts/make_dmg.sh` succeeds and prints the checksum.

## 2. Install from the DMG

Use the DMG itself, downloaded the way users get it (so it carries the quarantine flag), on a Mac or user account without a previous install where possible.

- [ ] The DMG opens and shows Pasted, the Applications link and "Read Me First.txt".
- [ ] After dragging to Applications, the first launch is blocked; the steps in "Read Me First.txt" open it. The checksum matches the `.sha256` file.
- [ ] The welcome window appears once, with the correct shortcut. Accessibility status changes to Allowed within a few seconds of granting it, without restarting.
- [ ] Upgrading over the previous release keeps history, pinboards, settings and (if enabled) encryption.
- [ ] Launch at login works after a restart.

## 3. Everyday use

- [ ] Copy text, a link, an image, a file in Finder and rich text from a browser; each appears with the right type and source app.
- [ ] Paste with Return, paste as plain text, Paste As transforms, quick paste with Command-1 to 9.
- [ ] Paste Stack in both orders.
- [ ] Copies from a password manager (1Password, Passwords) are not saved; with sensitive item expiry on, they are kept masked and disappear on time.
- [ ] Search finds exact and close matches; filters (`type:`, `app:`, `board:`) work.
- [ ] Statistics window: charts, Table view and Copy Data; devices filter.
- [ ] Import from Paste on a Mac that has Paste installed.

## 4. Windows, spaces and displays

Open the shelf with the shortcut in each situation, then paste into the frontmost app.

- [ ] A full-screen app (for example Safari or Keynote in full screen): the shelf appears over it on the same Space and pasting works.
- [ ] Multiple displays: the shelf opens on the display with the mouse pointer and is fully on screen; the Paste Stack panel opens on the main display; repeat with displays of different scale (Retina and non-Retina) and arrangement (side by side, stacked).
- [ ] Spaces: switch Spaces with the shelf closed and open it on each; it never pulls you back to another Space. Check "Displays have separate Spaces" both on and off.
- [ ] Stage Manager on: the shelf appears above the current stage, pasting goes to the app in the stage, and closing the shelf does not change stages.
- [ ] Mission Control and the Dock's app switching do not leave the shelf stuck open.
- [ ] Sleep and wake, and a display being unplugged while Pasted runs: the shortcut still works and the shelf opens on a connected display.

## 5. Secure input and privacy

- [ ] Secure input fields (a password field in Safari, the macOS login password prompt, Terminal with Secure Keyboard Entry on): the shortcut behaviour is acceptable (it may be blocked by macOS) and Pasted does not crash or capture what is typed. After leaving the field, everything works again.
- [ ] Pasting into a password field works when the shortcut is reachable, and leaves no extra copy in history.
- [ ] Screen sharing or recording with "visible during screen sharing" off hides the shelf from the recording.
- [ ] With encryption on, `pasted.db` and blob files contain no readable text.

## 6. Accessibility and appearance

- [ ] Light and dark mode: shelf, preview, Paste Stack, Settings, Statistics and welcome window are readable.
- [ ] VoiceOver: cards are read with type, app, age and position; sensitive items are not read out; search results and selection are announced.
- [ ] Reduce Motion: windows appear without animation. Increase Contrast: cards show borders.
- [ ] Keyboard only: every window can be used and closed without the mouse.
