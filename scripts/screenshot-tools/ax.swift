// Drives a running copy of Pasted through Accessibility, without synthetic
// keystrokes or clicks:
//   ax <pid> press            press the menu bar icon (opens the shelf)
//   ax <pid> menu <title>     open the menu bar icon's menu, choose an item
//   ax <pid> button <label>   press a button whose title or label contains <label>
//   ax <pid> tab <title>      choose a tab (in Settings)
import ApplicationServices
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let mode = CommandLine.arguments[2]
let app = AXUIElementCreateApplication(pid)

func attr(_ e: AXUIElement, _ a: String) -> CFTypeRef? {
    var v: CFTypeRef?
    AXUIElementCopyAttributeValue(e, a as CFString, &v)
    return v
}
func find(_ e: AXUIElement, _ depth: Int = 0, _ match: (AXUIElement) -> Bool) -> AXUIElement? {
    if match(e) { return e }
    if depth > 14 { return nil }
    for c in (attr(e, kAXChildrenAttribute) as? [AXUIElement]) ?? [] {
        if let f = find(c, depth + 1, match) { return f }
    }
    return nil
}
func statusItem() -> AXUIElement? {
    guard let bar = attr(app, "AXExtrasMenuBar") else { return nil }
    return (attr(bar as! AXUIElement, kAXChildrenAttribute) as? [AXUIElement])?.first
}
func fail(_ s: String) -> Never { FileHandle.standardError.write((s + "\n").data(using: .utf8)!); exit(1) }

switch mode {
case "press":
    guard let item = statusItem() else { fail("no menu bar icon") }
    _ = AXUIElementPerformAction(item, kAXPressAction as CFString)
case "menu":
    guard let item = statusItem() else { fail("no menu bar icon") }
    _ = AXUIElementPerformAction(item, kAXShowMenuAction as CFString)
    usleep(800_000)
    let title = CommandLine.arguments[3]
    guard let entry = find(app, 0, { (attr($0, kAXRoleAttribute) as? String) == kAXMenuItemRole && (attr($0, kAXTitleAttribute) as? String) == title }) else {
        fail("no menu item \(title)")
    }
    _ = AXUIElementPerformAction(entry, kAXPressAction as CFString)
case "button":
    let label = CommandLine.arguments[3]
    guard let b = find(app, 0, { e in
        guard (attr(e, kAXRoleAttribute) as? String) == kAXButtonRole else { return false }
        let t = (attr(e, kAXTitleAttribute) as? String ?? "") + " " + (attr(e, kAXDescriptionAttribute) as? String ?? "")
        return t.contains(label)
    }) else { fail("no button \(label)") }
    _ = AXUIElementPerformAction(b, kAXPressAction as CFString)
case "tab":
    let title = CommandLine.arguments[3]
    guard let t = find(app, 0, { (attr($0, kAXRoleAttribute) as? String) == kAXRadioButtonRole && (attr($0, kAXTitleAttribute) as? String) == title }) else {
        fail("no tab \(title)")
    }
    _ = AXUIElementPerformAction(t, kAXPressAction as CFString)
default:
    fail("unknown mode \(mode)")
}
