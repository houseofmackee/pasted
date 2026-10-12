#!/bin/sh
# Regenerates the README screenshots in docs/screenshots/ from synthetic
# demo data: a throwaway data folder, a copy of Pasted driven through
# Accessibility (no synthetic keystrokes) and captures of single windows,
# so nothing else on screen ends up in the images.
#
#   scripts/readme_screenshots.sh
#
# Needs Accessibility permission for the terminal running it, and network
# access for the link previews. Your own Pasted can keep running; the copy
# uses its own data folder and Keychain item.
set -eu
cd "$(dirname "$0")/.."

OUT=docs/screenshots
TOOLS=target/screenshot-tools
mkdir -p "$OUT" "$TOOLS"
swiftc -O scripts/screenshot-tools/windows.swift -o "$TOOLS/windows"
swiftc -O scripts/screenshot-tools/ax.swift -o "$TOOLS/ax"
cargo build --release
BIN=target/release/pasted

HOME_DIR=$(mktemp -d)
PID=""
cleanup() {
    [ -n "$PID" ] && kill "$PID" 2>/dev/null || true
    rm -rf "$HOME_DIR"
}
trap cleanup EXIT
export PASTED_HOME="$HOME_DIR" PASTED_KEYCHAIN_SERVICE=io.github.houseofmackee.pasted.screenshots
touch "$HOME_DIR/.welcomed"
cat > "$HOME_DIR/settings.toml" <<EOF
[general]
sounds = false
[privacy]
visible_during_screen_sharing = true
EOF

# Demo items, newest last: text|bundle id|app name|minutes ago.
NOW=$(($(date +%s) * 1000))
add() {
    id=$(printf '%s' "$1" | "$BIN" add | tail -1 | tr -dc '0-9')
    sqlite3 "$HOME_DIR/pasted.db" "
        INSERT OR IGNORE INTO apps (bundle_id, name) VALUES ('$2', '$3');
        UPDATE items SET app_id = (SELECT id FROM apps WHERE bundle_id = '$2'),
            created_at = $NOW - $4 * 60000, timestamp = $NOW - $4 * 60000,
            device = '$5' WHERE id = $id;"
    echo "$id"
}
# Older history for the statistics: a few weeks of everyday copies.
LINKS="https://en.wikipedia.org/wiki/Typography
https://en.wikipedia.org/wiki/Unix_philosophy
https://en.wikipedia.org/wiki/Keyboard_shortcut
https://en.wikipedia.org/wiki/Menu_bar
https://en.wikipedia.org/wiki/Optical_character_recognition
https://en.wikipedia.org/wiki/Cut,_copy,_and_paste
https://en.wikipedia.org/wiki/Macintosh
https://en.wikipedia.org/wiki/Rust_(programming_language)
https://en.wikipedia.org/wiki/SQLite
https://en.wikipedia.org/wiki/Clipboard_manager"
i=0
while [ $i -lt 140 ]; do
    case $((i % 7)) in
        0) app="com.apple.Safari|Safari"; text="$(echo "$LINKS" | sed -n "$((i / 7 % 10 + 1))p")?ref=$i" ;;
        1) app="com.apple.Notes|Notes"; text="Note $i: follow up on the design review" ;;
        2) app="com.apple.mail|Mail"; text="Re: invoice $i for October" ;;
        3) app="com.apple.Terminal|Terminal"; text="git log --oneline -n $i" ;;
        4) app="com.apple.TextEdit|TextEdit"; text="Draft paragraph $i about the launch" ;;
        5) app="com.apple.Safari|Safari"; text="Reminder $i: renew the domain before the end of the month" ;;
        *) app="com.apple.finder|Finder"; text="Quarterly report $i.pdf" ;;
    esac
    device="MacBook Pro"
    [ $((i % 4)) -eq 0 ] && device="Mac mini"
    minutes=$((600 + i * 280 + (i * 37 % 300)))
    add "$text" "${app%%|*}" "${app#*|}" "$minutes" "$device" > /dev/null
    i=$((i + 1))
done
add "Meeting notes: ship 0.3.0 on Friday, then look at sync between Macs." com.apple.Notes Notes 190 "MacBook Pro" > "$HOME_DIR/notes_id.tmp"
add "https://en.wikipedia.org/wiki/Clipboard_(computing)" com.apple.Safari Safari 150 "Mac mini" > /dev/null
add "https://www.rust-lang.org/learn" com.apple.Safari Safari 95 "MacBook Pro" > /dev/null
add "Grocery list: oat milk, eggs, coffee beans, lemons, basil" com.apple.Notes Notes 62 "MacBook Pro" > "$HOME_DIR/grocery_id.tmp"
add "https://developer.apple.com/documentation/appkit" com.apple.Safari Safari 41 "MacBook Pro" > /dev/null
add "#FF9F0A" com.apple.freeform Freeform 26 "MacBook Pro" > /dev/null
add 'fn main() {
    let total: u32 = (1..=10).sum();
    println!("{total}");
}' com.apple.Terminal Terminal 14 "MacBook Pro" > /dev/null
add "Quarterly roadmap review moved to Thursday at 10:00. Please bring the updated estimates for search and sync." com.apple.mail Mail 6 "MacBook Pro" > "$HOME_DIR/review_id.tmp"
add "https://github.com/houseofmackee/pasted" com.apple.Safari Safari 2 "MacBook Pro" > /dev/null
for f in "$HOME_DIR/notes_id.tmp" "$HOME_DIR/grocery_id.tmp" "$HOME_DIR/review_id.tmp"; do
    "$BIN" pin "$(cat "$f")" Work > /dev/null
    sqlite3 "$HOME_DIR/pasted.db" "UPDATE items SET use_count = 7 WHERE id = $(cat "$f");"
    rm -f "$f"
done
"$BIN" board create Links --search "type:link" > /dev/null
# Paste counts for "Most pasted", on the realistic items only.
sqlite3 "$HOME_DIR/pasted.db" "
    UPDATE items SET use_count = 12 WHERE text = 'https://github.com/houseofmackee/pasted';
    UPDATE items SET use_count = 9 WHERE text LIKE 'fn main()%';
    UPDATE items SET use_count = 5 WHERE text = '#FF9F0A';
    UPDATE items SET use_count = 3 WHERE text = 'https://developer.apple.com/documentation/appkit';"

# start APPEARANCE: runs the copy and waits for its link previews.
start() {
    PASTED_APPEARANCE=$1 "$BIN" > "$HOME_DIR/log.txt" 2>&1 &
    PID=$!
    sleep 4
}
stop() {
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
    PID=""
    sleep 1
}
# window TITLE|widest: the id of a window of the copy.
window() {
    if [ "$1" = widest ]; then
        "$TOOLS/windows" "$PID" | sort -k2 -n | tail -1 | cut -d' ' -f1
    else
        "$TOOLS/windows" "$PID" | grep " $1\$" | head -1 | cut -d' ' -f1
    fi
}
# capture WINDOW_ID FILE WIDTH: one window, without its shadow, scaled.
capture() {
    screencapture -o -x -l"$1" "$2"
    magick "$2" -resize "$3x>" -strip "$2"
    echo "Wrote $2"
}

start light
sleep 8 # link previews are fetched in the background
"$TOOLS/ax" "$PID" press
sleep 2
capture "$(window widest)" "$OUT/shelf.png" 1800
"$TOOLS/ax" "$PID" press
"$TOOLS/ax" "$PID" menu "Statistics…"
sleep 2
capture "$(window "Pasted Statistics")" "$OUT/statistics.png" 1200
# Only the shelf needed to be capturable; show Settings as installed.
cat > "$HOME_DIR/settings.toml" <<EOF
[general]
sounds = false
EOF
sleep 4 # the app notices settings.toml changes within a few seconds
"$TOOLS/ax" "$PID" menu "Settings…"
sleep 2
"$TOOLS/ax" "$PID" tab "Privacy"
sleep 1
capture "$(window "Pasted Settings")" "$OUT/settings-privacy.png" 1000
stop

cat > "$HOME_DIR/settings.toml" <<EOF
[general]
sounds = false
[privacy]
visible_during_screen_sharing = true
EOF
start dark
"$TOOLS/ax" "$PID" press
sleep 2
"$TOOLS/ax" "$PID" button "Links smart pinboard"
sleep 2
capture "$(window widest)" "$OUT/shelf-smart-dark.png" 1800
stop
