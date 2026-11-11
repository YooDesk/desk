import AppKit
import Carbon

// 本机验收窗口。只记录固定测试串是否到达，不记录用户输入。
final class Witness: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    let result = URL(fileURLWithPath: CommandLine.arguments[1])
    let marker = CommandLine.arguments[2]
    var window: NSWindow?
    var input: NSTextField?
    var clicked = false
    var originalInputSource: TISInputSource?
    let pinyin = CommandLine.arguments.contains("--pinyin")
    let keymap = CommandLine.arguments.contains("--keymap")
    var observedKeys = Set<UInt16>()
    var observedCommand = Set<UInt16>()
    var observedEditing = Set<UInt16>()
    var observedRawControlCopy = false
    var eventMonitor: Any?

    func applicationWillTerminate(_ notification: Notification) {
        if let eventMonitor { NSEvent.removeMonitor(eventMonitor) }
        if let originalInputSource { TISSelectInputSource(originalInputSource) }
    }

    func activateTestInputSource() -> Bool {
        let id = pinyin ? "com.apple.inputmethod.SCIM.ITABC" : "com.apple.keylayout.ABC"
        let filter = [kTISPropertyInputSourceID as String: id]
        guard let sources = TISCreateInputSourceList(filter as CFDictionary, false)?
            .takeRetainedValue() as? [TISInputSource], let source = sources.first else { return false }
        originalInputSource = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue()
        return TISSelectInputSource(source) == noErr
    }

    @objc func acknowledgeMouse() {
        clicked = true
        if let input { window?.makeFirstResponder(input) }
        try? "mouse-ok\n".write(to: result, atomically: true, encoding: .utf8)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let frame = NSRect(x: 0, y: 0, width: 520, height: 160)
        let window = NSWindow(contentRect: frame, styleMask: [.titled, .closable],
                              backing: .buffered, defer: false)
        window.title = "YooDesk Input Test"
        window.level = .floating
        let input = NSTextField(frame: NSRect(x: 30, y: 66, width: 460, height: 28))
        input.placeholderString = "YooDesk automated keyboard witness"
        input.delegate = self
        window.contentView?.addSubview(input)
        let button = NSButton(title: "YooDesk mouse witness", target: self,
                              action: #selector(acknowledgeMouse))
        button.frame = NSRect(x: 30, y: 104, width: 230, height: 28)
        window.contentView?.addSubview(button)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        window.makeFirstResponder(input)
        self.window = window
        self.input = input
        if !activateTestInputSource() {
            try? "input-source-unavailable\n".write(to: result, atomically: true, encoding: .utf8)
            NSApp.terminate(nil)
            return
        }
        if keymap {
            eventMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { [weak self] event in
                guard let self else { return event }
                return self.observeMappedKey(event)
            }
        }
        Timer.scheduledTimer(withTimeInterval: 20, repeats: false) { _ in NSApp.terminate(nil) }
        let global = window.convertToScreen(button.convert(button.bounds, to: nil))
        let mainTop = NSScreen.screens.first?.frame.maxY ?? 0
        try? "ready \(Int(global.midX)) \(Int(mainTop - global.midY))\n"
            .write(to: result, atomically: true, encoding: .utf8)
    }

    func observeMappedKey(_ event: NSEvent) -> NSEvent? {
        let code = event.keyCode
        if event.type == .flagsChanged && event.modifierFlags.contains(.command) {
            observedCommand.insert(code)
        }
        if event.type == .keyDown {
            observedKeys.insert(code)
            if code == 8 && event.modifierFlags.contains(.control) && !event.modifierFlags.contains(.command) {
                observedRawControlCopy = true
                return nil
            }
            if event.modifierFlags.contains(.command) && [8, 9].contains(code) {
                observedEditing.insert(code)
                // Verify the shortcut without overwriting the real user's pasteboard.
                if [65, 69, 76, 83].allSatisfy(observedKeys.contains)
                    && [54, 55].allSatisfy(observedCommand.contains)
                    && [8, 9].allSatisfy(observedEditing.contains) && observedRawControlCopy {
                    try? "keymap-ok\n".write(to: result, atomically: true, encoding: .utf8)
                    DispatchQueue.main.async { NSApp.terminate(nil) }
                    return nil
                }
            }
        }
        let keypad = [65, 69, 76, 83].filter(observedKeys.contains)
        let command = [54, 55].filter(observedCommand.contains)
        let editing = [8, 9].filter(observedEditing.contains)
        try? "keys=\(keypad) command=\(command) editing=\(editing) rawControlCopy=\(observedRawControlCopy)\n"
            .write(to: result, atomically: true, encoding: .utf8)
        if event.type == .keyDown && event.modifierFlags.contains(.command) && [8, 9].contains(code) {
            return nil
        }
        return event
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let field = notification.object as? NSTextField, field.stringValue.contains(marker) else { return }
        try? (clicked ? "both-ok\n" : "keyboard-ok\n")
            .write(to: result, atomically: true, encoding: .utf8)
        NSApp.terminate(nil)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

guard CommandLine.arguments.count == 3 ||
    (CommandLine.arguments.count == 4 && ["--pinyin", "--keymap"].contains(CommandLine.arguments[3])) else { exit(2) }
let app = NSApplication.shared
let delegate = Witness()
app.setActivationPolicy(.regular)
app.delegate = delegate
app.run()
