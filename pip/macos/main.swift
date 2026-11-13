import AppKit
import WebKit

// Floating, non-activating picture-in-picture window for an agent-browser session.
// Usage: agent-browser-pip [url]   (default http://localhost:4848/pip)
// Normally started by `agent-browser pip`.

let defaultURL = "http://localhost:4848/pip"
let margin: CGFloat = 16
let dragStripHeight: CGFloat = 20
let resizeHandleSize: CGFloat = 14
let defaultWidth: CGFloat = 400
let minWidth: CGFloat = 240
let maxWidth: CGFloat = 640
let minHeight: CGFloat = 180
let maxHeight: CGFloat = 400
let maxScreenHeightFraction: CGFloat = 0.35
let initialAspect: CGFloat = 16.0 / 9.0
let userGestureWindow: TimeInterval = 2.0
let cornerKey = "corner"
let widthKey = "width"

enum Corner: String {
  case topLeft, topRight, bottomLeft, bottomRight

  var isTop: Bool { self == .topLeft || self == .topRight }
  var isRight: Bool { self == .topRight || self == .bottomRight }

  func origin(for size: NSSize, in area: NSRect) -> NSPoint {
    NSPoint(
      x: isRight ? area.maxX - size.width - margin : area.minX + margin,
      y: isTop ? area.maxY - size.height - margin : area.minY + margin
    )
  }

  static func nearest(to frame: NSRect, in area: NSRect) -> Corner {
    switch (frame.midY > area.midY, frame.midX > area.midX) {
    case (true, true): return .topRight
    case (true, false): return .topLeft
    case (false, true): return .bottomRight
    case (false, false): return .bottomLeft
    }
  }
}

/// Size for a frame aspect: width wins, height is clamped, extra space letterboxes.
/// While the page shows its bar, the frame sits below it, so the panel adds the bar height.
func fittedSize(width requested: CGFloat, aspect: CGFloat, screen: NSRect, bar: Bool) -> NSSize {
  let barHeight = bar ? dragStripHeight : 0
  let heightCap = min(maxHeight, screen.height * maxScreenHeightFraction)
  var width = min(max(requested, minWidth), maxWidth)
  var height = width / aspect + barHeight
  if height > heightCap {
    height = heightCap
    width = max(minWidth, (height - barHeight) * aspect)
  }
  height = max(height, minHeight)
  return NSSize(width: width.rounded(), height: height.rounded())
}

/// The panel is never active, so the web view has to take the first click.
final class PipWebView: WKWebView {
  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

final class PipPanel: NSPanel {
  override var canBecomeKey: Bool { true }
  override var canBecomeMain: Bool { false }
}

/// WKWebView swallows mouse events, so moving the window needs a native view on top.
final class DragStrip: NSView {
  var onDragEnd: (() -> Void)?
  var onDoubleClick: (() -> Void)?
  var onClose: (() -> Void)?

  // The panel never activates, so the first click has to reach the strip.
  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

  override func mouseDown(with event: NSEvent) {
    if event.clickCount == 2 {
      onDoubleClick?()
      return
    }
    beginDrag(with: event)
  }

  func beginDrag(with event: NSEvent) {
    window?.performDrag(with: event)
    waitForMouseUp { [weak self] in self?.onDragEnd?() }
  }

  override func rightMouseDown(with event: NSEvent) {
    let menu = NSMenu()
    menu.addItem(withTitle: "Close", action: #selector(close), keyEquivalent: "").target = self
    NSMenu.popUpContextMenu(menu, with: event, for: self)
  }

  @objc private func close() { onClose?() }

  override func resetCursorRects() { addCursorRect(bounds, cursor: .openHand) }
}

/// performDrag can return before the drag finishes, so wait for the button to lift.
func waitForMouseUp(_ done: @escaping () -> Void) {
  Timer.scheduledTimer(withTimeInterval: 1.0 / 60.0, repeats: true) { timer in
    guard NSEvent.pressedMouseButtons & 1 == 0 else { return }
    timer.invalidate()
    done()
  }
}

/// Bottom-corner handle: dragging changes the width, the height follows the aspect.
final class ResizeHandle: NSView {
  let growsLeft: Bool
  var onResize: ((CGFloat) -> Void)?
  var onResizeEnd: (() -> Void)?
  private var startWidth: CGFloat = 0
  private var startX: CGFloat = 0

  init(frame: NSRect, growsLeft: Bool) {
    self.growsLeft = growsLeft
    super.init(frame: frame)
  }

  required init?(coder: NSCoder) { fatalError("unused") }

  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

  // AppKit can route later drags here after another gesture swallowed its
  // mouseDown (Cmd+drag), so only resize between this view's own down and up.
  private var resizing = false

  override func mouseDown(with event: NSEvent) {
    resizing = true
    startWidth = window?.frame.width ?? 0
    startX = NSEvent.mouseLocation.x
  }

  override func mouseDragged(with event: NSEvent) {
    guard resizing else { return }
    let dx = NSEvent.mouseLocation.x - startX
    onResize?(startWidth + (growsLeft ? -dx : dx))
  }

  override func mouseUp(with event: NSEvent) {
    guard resizing else { return }
    resizing = false
    onResizeEnd?()
  }

  override func resetCursorRects() { addCursorRect(bounds, cursor: .resizeLeftRight) }
}

final class AppDelegate: NSObject, NSApplicationDelegate, WKScriptMessageHandler, WKNavigationDelegate {
  private var panel: PipPanel!
  private var webView: WKWebView!
  private var strip: DragStrip!
  private let url: URL
  private var corner: Corner
  private var aspect = initialAspect
  private var preferredWidth: CGFloat
  private var aspectDebounce: Timer?
  private var previousApp: NSRunningApplication?
  private var interacting = false
  private var barVisible = true
  private var lastUserGesture: TimeInterval = 0

  init(url: URL) {
    self.url = url
    corner = Corner(rawValue: UserDefaults.standard.string(forKey: cornerKey) ?? "") ?? .bottomRight
    let stored = UserDefaults.standard.double(forKey: widthKey)
    preferredWidth = stored > 0 ? CGFloat(stored) : defaultWidth
  }

  private var screenArea: NSRect {
    panel?.screen?.visibleFrame ?? NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
  }

  func applicationDidFinishLaunching(_ notification: Notification) {
    let size = fittedSize(width: preferredWidth, aspect: aspect, screen: screenArea, bar: barVisible)
    panel = PipPanel(
      contentRect: NSRect(origin: corner.origin(for: size, in: screenArea), size: size),
      styleMask: [.borderless, .nonactivatingPanel],
      backing: .buffered,
      defer: false
    )
    panel.level = .floating
    panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
    panel.hidesOnDeactivate = false
    panel.isOpaque = false
    panel.backgroundColor = .clear
    panel.hasShadow = true

    let container = NSView(frame: NSRect(origin: .zero, size: size))
    container.wantsLayer = true
    container.layer?.cornerRadius = 10
    container.layer?.masksToBounds = true
    container.layer?.backgroundColor = NSColor.black.cgColor

    let configuration = WKWebViewConfiguration()
    configuration.userContentController.add(self, name: "pip")
    webView = PipWebView(frame: container.bounds, configuration: configuration)
    webView.autoresizingMask = [.width, .height]
    webView.underPageBackgroundColor = .black
    webView.navigationDelegate = self
    webView.load(URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData))
    container.addSubview(webView)

    strip = DragStrip(frame: NSRect(x: 0, y: size.height - dragStripHeight, width: size.width, height: dragStripHeight))
    strip.autoresizingMask = [.width, .minYMargin]
    strip.onDragEnd = { [weak self] in self?.snapToNearestCorner() }
    strip.onDoubleClick = { [weak self] in self?.openDashboard() }
    strip.onClose = { NSApp.terminate(nil) }
    container.addSubview(strip)

    for growsLeft in [true, false] {
      let handle = ResizeHandle(
        frame: NSRect(
          x: growsLeft ? 0 : size.width - resizeHandleSize, y: 0,
          width: resizeHandleSize, height: resizeHandleSize),
        growsLeft: growsLeft
      )
      handle.autoresizingMask = growsLeft ? [.maxXMargin, .maxYMargin] : [.minXMargin, .maxYMargin]
      handle.onResize = { [weak self] width in self?.resizeFromHandle(toWidth: width, growsLeft: growsLeft) }
      handle.onResizeEnd = { [weak self] in
        guard let self else { return }
        preferredWidth = panel.frame.width
        UserDefaults.standard.set(Double(preferredWidth), forKey: widthKey)
        snapToNearestCorner()
      }
      container.addSubview(handle)
    }

    panel.contentView = container
    panel.orderFrontRegardless()
    installCommandDrag()
    installUserGestureTracking()
  }

  /// Cmd+drag anywhere moves the panel; the 20 px strip is a small target.
  private func installCommandDrag() {
    NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
      guard let self, event.window === panel, event.modifierFlags.contains(.command) else { return event }
      strip.beginDrag(with: event)
      return nil
    }
  }

  /// Records clicks and Option-hovers that land on the page, so keyboard focus
  /// only ever follows something the user did in this window.
  private func installUserGestureTracking() {
    NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .mouseMoved]) { [weak self] event in
      self?.noteUserGesture(event)
      return event
    }
  }

  private func noteUserGesture(_ event: NSEvent) {
    guard event.window === panel else { return }
    guard event.type != .mouseMoved || event.modifierFlags.contains(.option) else { return }
    guard let content = panel.contentView,
          let hit = content.hitTest(content.convert(event.locationInWindow, from: nil))
    else { return }
    var view: NSView? = hit
    while let current = view {
      if current === webView {
        lastUserGesture = ProcessInfo.processInfo.systemUptime
        return
      }
      view = current.superview
    }
  }

  // MARK: Page messages

  /// The bridge is visible to every frame the page loads, so each message has
  /// to come from the dashboard document itself.
  private func isDashboardFrame(_ frame: WKFrameInfo) -> Bool {
    guard frame.isMainFrame else { return false }
    let origin = frame.securityOrigin
    return origin.protocol.lowercased() == "http"
      && isLoopbackHost(origin.host)
      && origin.port == dashboardPort
  }

  func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
    guard isDashboardFrame(message.frameInfo),
          let body = message.body as? [String: Any],
          let type = body["type"] as? String
    else { return }
    switch type {
    case "aspect":
      if let value = body["value"] as? Double, value > 0 { scheduleAspect(CGFloat(value)) }
    case "interact":
      setInteracting((body["on"] as? Bool) ?? false)
    case "idle":
      let idle = (body["idle"] as? Bool) ?? false
      NSAnimationContext.runAnimationGroup { context in
        context.duration = 0.15
        panel.animator().alphaValue = idle ? 0.85 : 1.0
      }
    case "bar":
      let visible = (body["visible"] as? Bool) ?? true
      guard visible != barVisible else { return }
      barVisible = visible
      resize(toWidth: panel.frame.width, animated: true)
    case "close":
      NSApp.terminate(nil)
    default:
      break
    }
  }

  private func scheduleAspect(_ value: CGFloat) {
    guard abs(value - aspect) > 0.01 else { return }
    aspectDebounce?.invalidate()
    aspectDebounce = Timer.scheduledTimer(withTimeInterval: 0.3, repeats: false) { [weak self] _ in
      guard let self else { return }
      aspect = value
      resize(toWidth: preferredWidth, animated: true)
    }
  }

  /// Takes keyboard focus without activating the app, then hands it back.
  /// Only the verified dashboard page may ask for it, and only right after a
  /// gesture the user made on the frame; page script alone cannot move focus.
  private func setInteracting(_ on: Bool) {
    guard on != interacting else { return }
    if on {
      guard ProcessInfo.processInfo.systemUptime - lastUserGesture < userGestureWindow
      else { return }
      interacting = true
      previousApp = NSWorkspace.shared.frontmostApplication
      panel.becomesKeyOnlyIfNeeded = false
      panel.makeKey()
      panel.makeFirstResponder(webView)
    } else {
      interacting = false
      panel.becomesKeyOnlyIfNeeded = true
      if let previousApp, previousApp.processIdentifier != ProcessInfo.processInfo.processIdentifier {
        previousApp.activate()
      }
      previousApp = nil
    }
  }

  // MARK: Navigation

  /// Navigation stays on the dashboard the CLI verified; links open elsewhere.
  func webView(
    _ webView: WKWebView,
    decidePolicyFor navigationAction: WKNavigationAction,
    decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
  ) {
    guard let target = navigationAction.request.url else {
      decisionHandler(.cancel)
      return
    }
    if isDashboardURL(target) {
      decisionHandler(.allow)
      return
    }
    decisionHandler(.cancel)
    if navigationAction.navigationType == .linkActivated,
       let scheme = target.scheme?.lowercased(), scheme == "http" || scheme == "https" {
      NSWorkspace.shared.open(target)
    }
  }

  private var dashboardPort: Int { url.port ?? 80 }

  private func isLoopbackHost(_ host: String?) -> Bool {
    guard let host = host?.lowercased() else { return false }
    return host == "localhost" || host == "127.0.0.1"
  }

  private func isDashboardURL(_ candidate: URL?) -> Bool {
    guard let candidate, candidate.scheme == "http", let host = candidate.host else {
      return false
    }
    return isLoopbackHost(host) && (candidate.port ?? 80) == dashboardPort
  }

  // MARK: Geometry

  /// Keeps the snapped corner anchored while the size changes.
  private func resize(toWidth width: CGFloat, animated: Bool) {
    let size = fittedSize(width: width, aspect: aspect, screen: screenArea, bar: barVisible)
    let frame = panel.frame
    let origin = NSPoint(
      x: corner.isRight ? frame.maxX - size.width : frame.minX,
      y: corner.isTop ? frame.maxY - size.height : frame.minY
    )
    let target = NSRect(origin: origin, size: size)
    guard animated else {
      panel.setFrame(target, display: true)
      return
    }
    NSAnimationContext.runAnimationGroup { context in
      context.duration = 0.2
      context.timingFunction = CAMediaTimingFunction(name: .easeOut)
      panel.animator().setFrame(target, display: true)
    }
  }

  /// The dragged edge follows the cursor; the opposite side and the top stay put.
  private func resizeFromHandle(toWidth width: CGFloat, growsLeft: Bool) {
    let size = fittedSize(width: width, aspect: aspect, screen: screenArea, bar: barVisible)
    let frame = panel.frame
    let origin = NSPoint(x: growsLeft ? frame.maxX - size.width : frame.minX, y: frame.maxY - size.height)
    panel.setFrame(NSRect(origin: origin, size: size), display: true)
  }

  private func snapToNearestCorner() {
    corner = Corner.nearest(to: panel.frame, in: screenArea)
    UserDefaults.standard.set(corner.rawValue, forKey: cornerKey)
    let target = NSRect(origin: corner.origin(for: panel.frame.size, in: screenArea), size: panel.frame.size)
    NSAnimationContext.runAnimationGroup { context in
      context.duration = 0.2
      context.timingFunction = CAMediaTimingFunction(name: .easeOut)
      panel.animator().setFrame(target, display: true)
    }
  }

  private func openDashboard() {
    var components = URLComponents(url: url, resolvingAgainstBaseURL: false)
    components?.path = "/"
    if let dashboard = components?.url { NSWorkspace.shared.open(dashboard) }
  }
}

let target = CommandLine.arguments.dropFirst().first ?? defaultURL
guard let url = URL(string: target) else {
  FileHandle.standardError.write("agent-browser-pip: invalid url \(target)\n".data(using: .utf8)!)
  exit(2)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = AppDelegate(url: url)
app.delegate = delegate
app.run()
