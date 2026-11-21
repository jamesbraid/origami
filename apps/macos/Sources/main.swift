// Origami for macOS.
//   Origami                         the app
//   Origami --machines DIR          use DIR as the library (repeatable)
//   Origami --selftest DIR OUT      headless: no window is ever shown
//   Origami --docs-shots DIR OUT    the pictures the documentation shows, from real windows
//   Origami --selftest-display DIR OUT   dev builds: the real window against the behaviour table
// The core finds its QEMU beside the executable (ORIGAMI_RUNTIME_DIR overrides).

import AppKit

var args = Array(CommandLine.arguments.dropFirst())
// `open App.app --args ...` also passes launch flags; keep ours.
args = args.filter { !$0.hasPrefix("-psn_") && !$0.hasPrefix("-NS") && !$0.hasPrefix("-Apple") }

let nsApp = NSApplication.shared

if args.first != "--selftest" { PrefsMigration.run() }

#if FAKE
// The real machine window on screen, checked against the core's behaviour table; never in a release build, which has the QEMU engine.
if args.first == "--selftest-display" {
    guard args.count >= 3 else {
        FileHandle.standardError.write(Data("usage: Origami --selftest-display MACHINES_DIR OUTDIR\n".utf8))
        exit(2)
    }
    nsApp.setActivationPolicy(.regular)
    nsApp.finishLaunching()
    MainActor.assumeIsolated { runDisplaySelfTest(library: args[1], out: args[2]) }
}
#endif

if args.first == "--selftest" {
    guard args.count >= 3 else {
        FileHandle.standardError.write(Data("usage: Origami --selftest MACHINES_DIR OUTDIR\n".utf8))
        exit(2)
    }
    nsApp.setActivationPolicy(.prohibited)
    MainActor.assumeIsolated { runSelfTest(library: args[1], out: args[2]) }
} else if args.first == "--docs-shots" {
    guard args.count >= 3 else {
        FileHandle.standardError.write(Data("usage: Origami --docs-shots MACHINES_DIR OUTDIR\n".utf8))
        exit(2)
    }
    nsApp.setActivationPolicy(.regular)
    nsApp.finishLaunching()
    MainActor.assumeIsolated { runDocsShots(library: args[1], out: args[2]) }
} else {
    nsApp.setActivationPolicy(.regular)
    // From a launcher there is no terminal: keep a log.
    if isatty(2) == 0 {
        freopen(NSHomeDirectory() + "/Library/Logs/Origami.log", "a", stderr)
    }
    MainActor.assumeIsolated {
        Shared.model = AppModel(root: Launch.root(arguments: args))
    }
    MainActor.assumeIsolated { OrigamiApp.main() }
}
