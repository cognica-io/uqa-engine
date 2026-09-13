//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data(("error: " + message + "\n").utf8))
    exit(1)
}

// Foundation applies the process policy when spawning the executable. Setting
// the launcher's pthread QoS before exec does not preserve it in the new image.
let arguments = Array(CommandLine.arguments.dropFirst())
guard arguments.count >= 2 else {
    fail("expected a new metadata path and an executable")
}
let metadata = URL(fileURLWithPath: arguments[0])
guard !FileManager.default.fileExists(atPath: metadata.path) else {
    fail("refusing to overwrite scheduling metadata")
}
let child = Process()
child.executableURL = URL(fileURLWithPath: arguments[1])
child.arguments = Array(arguments.dropFirst(2))
child.qualityOfService = .userInteractive
do {
    try child.run()
    let record: [String: Any] = [
        "policy": "macos_foundation_user_interactive",
        "requested_qos_class": child.qualityOfService.rawValue,
        "child_pid": child.processIdentifier,
    ]
    do {
        try JSONSerialization.data(withJSONObject: record, options: [.sortedKeys])
            .write(to: metadata, options: .withoutOverwriting)
    } catch {
        child.terminate()
        child.waitUntilExit()
        throw error
    }
    child.waitUntilExit()
    if child.terminationReason == .uncaughtSignal {
        exit(128 + child.terminationStatus)
    }
    exit(child.terminationStatus)
} catch {
    fail(String(describing: error))
}
