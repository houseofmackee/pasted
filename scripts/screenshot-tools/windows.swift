// Prints "id width height title" for each on-screen window of a process.
// Used by scripts/readme_screenshots.sh to capture single windows.
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid {
    let b = w[kCGWindowBounds as String] as! [String: CGFloat]
    let title = w[kCGWindowName as String] as? String ?? ""
    print(w[kCGWindowNumber as String]!, Int(b["Width"]!), Int(b["Height"]!), title)
}
