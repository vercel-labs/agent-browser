# Fish completion for agent-browser
# https://github.com/vercel-labs/agent-browser

# disable file completions by default
complete -c agent-browser -f

# helper: true when no subcommand has been given yet
function __agent_browser_needs_command
    set -l cmd (commandline -opc)
    test (count $cmd) -eq 1
end

# helper: true when the given subcommand is active
function __agent_browser_using_command
    set -l cmd (commandline -opc)
    test (count $cmd) -gt 1; and test "$cmd[2]" = "$argv[1]"
end

# ── top-level commands ──

# core
complete -c agent-browser -n __agent_browser_needs_command -a open        -d 'Navigate to URL'
complete -c agent-browser -n __agent_browser_needs_command -a click       -d 'Click element'
complete -c agent-browser -n __agent_browser_needs_command -a dblclick    -d 'Double-click element'
complete -c agent-browser -n __agent_browser_needs_command -a type        -d 'Type into element'
complete -c agent-browser -n __agent_browser_needs_command -a fill        -d 'Clear and fill element'
complete -c agent-browser -n __agent_browser_needs_command -a press       -d 'Press key'
complete -c agent-browser -n __agent_browser_needs_command -a keyboard    -d 'Keyboard actions'
complete -c agent-browser -n __agent_browser_needs_command -a hover       -d 'Hover element'
complete -c agent-browser -n __agent_browser_needs_command -a focus       -d 'Focus element'
complete -c agent-browser -n __agent_browser_needs_command -a check       -d 'Check checkbox'
complete -c agent-browser -n __agent_browser_needs_command -a uncheck     -d 'Uncheck checkbox'
complete -c agent-browser -n __agent_browser_needs_command -a select      -d 'Select dropdown option'
complete -c agent-browser -n __agent_browser_needs_command -a drag        -d 'Drag and drop'
complete -c agent-browser -n __agent_browser_needs_command -a upload      -d 'Upload files'
complete -c agent-browser -n __agent_browser_needs_command -a download    -d 'Download file'
complete -c agent-browser -n __agent_browser_needs_command -a scroll      -d 'Scroll page'
complete -c agent-browser -n __agent_browser_needs_command -a scrollintoview -d 'Scroll element into view'
complete -c agent-browser -n __agent_browser_needs_command -a wait        -d 'Wait for element or time'
complete -c agent-browser -n __agent_browser_needs_command -a screenshot  -d 'Take screenshot'
complete -c agent-browser -n __agent_browser_needs_command -a pdf         -d 'Save as PDF'
complete -c agent-browser -n __agent_browser_needs_command -a snapshot    -d 'Accessibility tree with refs'
complete -c agent-browser -n __agent_browser_needs_command -a eval        -d 'Run JavaScript'
complete -c agent-browser -n __agent_browser_needs_command -a connect     -d 'Connect via CDP'
complete -c agent-browser -n __agent_browser_needs_command -a close       -d 'Close browser'
# navigation
complete -c agent-browser -n __agent_browser_needs_command -a back        -d 'Go back'
complete -c agent-browser -n __agent_browser_needs_command -a forward     -d 'Go forward'
complete -c agent-browser -n __agent_browser_needs_command -a reload      -d 'Reload page'
# compound
complete -c agent-browser -n __agent_browser_needs_command -a get         -d 'Get element info'
complete -c agent-browser -n __agent_browser_needs_command -a is          -d 'Check element state'
complete -c agent-browser -n __agent_browser_needs_command -a find        -d 'Find elements'
complete -c agent-browser -n __agent_browser_needs_command -a mouse       -d 'Mouse actions'
complete -c agent-browser -n __agent_browser_needs_command -a set         -d 'Browser settings'
complete -c agent-browser -n __agent_browser_needs_command -a network     -d 'Network actions'
complete -c agent-browser -n __agent_browser_needs_command -a storage     -d 'Manage web storage'
complete -c agent-browser -n __agent_browser_needs_command -a cookies     -d 'Manage cookies'
complete -c agent-browser -n __agent_browser_needs_command -a tab         -d 'Manage tabs'
complete -c agent-browser -n __agent_browser_needs_command -a auth        -d 'Auth vault'
complete -c agent-browser -n __agent_browser_needs_command -a session     -d 'Session management'
complete -c agent-browser -n __agent_browser_needs_command -a dashboard   -d 'Dashboard server'
complete -c agent-browser -n __agent_browser_needs_command -a stream      -d 'WebSocket streaming'
complete -c agent-browser -n __agent_browser_needs_command -a diff        -d 'Compare pages/snapshots'
complete -c agent-browser -n __agent_browser_needs_command -a state       -d 'Manage browser state'
# debug
complete -c agent-browser -n __agent_browser_needs_command -a trace       -d 'Record trace'
complete -c agent-browser -n __agent_browser_needs_command -a profiler    -d 'Record profile'
complete -c agent-browser -n __agent_browser_needs_command -a record      -d 'Video recording'
complete -c agent-browser -n __agent_browser_needs_command -a console     -d 'View console logs'
complete -c agent-browser -n __agent_browser_needs_command -a errors      -d 'View page errors'
complete -c agent-browser -n __agent_browser_needs_command -a highlight   -d 'Highlight element'
complete -c agent-browser -n __agent_browser_needs_command -a inspect     -d 'Open Chrome DevTools'
complete -c agent-browser -n __agent_browser_needs_command -a clipboard   -d 'Clipboard operations'
# confirmation
complete -c agent-browser -n __agent_browser_needs_command -a confirm     -d 'Approve pending action'
complete -c agent-browser -n __agent_browser_needs_command -a deny        -d 'Deny pending action'
# iOS
complete -c agent-browser -n __agent_browser_needs_command -a tap         -d 'Touch element (iOS)'
complete -c agent-browser -n __agent_browser_needs_command -a swipe       -d 'Swipe gesture (iOS)'
complete -c agent-browser -n __agent_browser_needs_command -a device      -d 'iOS device management'
# dialog / frame / window
complete -c agent-browser -n __agent_browser_needs_command -a dialog      -d 'Dialog management'
complete -c agent-browser -n __agent_browser_needs_command -a frame       -d 'Switch frame context'
complete -c agent-browser -n __agent_browser_needs_command -a window      -d 'Window management'
# batch
complete -c agent-browser -n __agent_browser_needs_command -a batch       -d 'Execute commands from stdin'
# setup
complete -c agent-browser -n __agent_browser_needs_command -a install     -d 'Install browser binaries'
complete -c agent-browser -n __agent_browser_needs_command -a upgrade     -d 'Upgrade to latest version'

# ── subcommands ──

# get
complete -c agent-browser -n '__agent_browser_using_command get' -a 'text html value attr title url count box styles cdp-url'

# is
complete -c agent-browser -n '__agent_browser_using_command is' -a 'visible enabled checked'

# find
complete -c agent-browser -n '__agent_browser_using_command find' -a 'role text label placeholder alt title testid first last nth'

# mouse
complete -c agent-browser -n '__agent_browser_using_command mouse' -a 'move down up wheel'

# set
complete -c agent-browser -n '__agent_browser_using_command set' -a 'viewport device geo offline headers credentials media'

# network
complete -c agent-browser -n '__agent_browser_using_command network' -a 'route unroute requests har'

# storage
complete -c agent-browser -n '__agent_browser_using_command storage' -a 'local session'

# tab
complete -c agent-browser -n '__agent_browser_using_command tab' -a 'new list close'

# cookies
complete -c agent-browser -n '__agent_browser_using_command cookies' -a 'get set clear'

# auth
complete -c agent-browser -n '__agent_browser_using_command auth' -a 'save login list show delete'

# session
complete -c agent-browser -n '__agent_browser_using_command session' -a list

# dashboard
complete -c agent-browser -n '__agent_browser_using_command dashboard' -a 'start stop install'

# stream
complete -c agent-browser -n '__agent_browser_using_command stream' -a 'enable disable status'

# diff
complete -c agent-browser -n '__agent_browser_using_command diff' -a 'snapshot screenshot url'

# state
complete -c agent-browser -n '__agent_browser_using_command state' -a 'save load list clear show clean rename'

# trace
complete -c agent-browser -n '__agent_browser_using_command trace' -a 'start stop'

# profiler
complete -c agent-browser -n '__agent_browser_using_command profiler' -a 'start stop'

# record
complete -c agent-browser -n '__agent_browser_using_command record' -a 'start stop restart'

# clipboard
complete -c agent-browser -n '__agent_browser_using_command clipboard' -a 'read write copy paste'

# device
complete -c agent-browser -n '__agent_browser_using_command device' -a list

# dialog
complete -c agent-browser -n '__agent_browser_using_command dialog' -a 'accept dismiss status'

# keyboard
complete -c agent-browser -n '__agent_browser_using_command keyboard' -a 'type inserttext'

# window
complete -c agent-browser -n '__agent_browser_using_command window' -a new

# scroll
complete -c agent-browser -n '__agent_browser_using_command scroll' -a 'up down left right'

# ── command-specific options ──

# snapshot
complete -c agent-browser -n '__agent_browser_using_command snapshot' -s i -l interactive -d 'Only interactive elements'
complete -c agent-browser -n '__agent_browser_using_command snapshot' -s c -l compact     -d 'Remove empty elements'
complete -c agent-browser -n '__agent_browser_using_command snapshot' -s d -l depth       -d 'Limit tree depth' -x
complete -c agent-browser -n '__agent_browser_using_command snapshot' -s s -l selector    -d 'Scope to CSS selector' -x

# wait
complete -c agent-browser -n '__agent_browser_using_command wait' -l url      -d 'Wait for URL pattern' -x
complete -c agent-browser -n '__agent_browser_using_command wait' -l load     -d 'Wait for load state' -xa 'load domcontentloaded networkidle'
complete -c agent-browser -n '__agent_browser_using_command wait' -l fn       -d 'Wait for JS function' -x
complete -c agent-browser -n '__agent_browser_using_command wait' -l text     -d 'Wait for text content' -x
complete -c agent-browser -n '__agent_browser_using_command wait' -l download -d 'Wait for download'

# close
complete -c agent-browser -n '__agent_browser_using_command close' -l all -d 'Close all sessions'

# install
complete -c agent-browser -n '__agent_browser_using_command install' -l with-deps -d 'Install system dependencies'

# console / errors
complete -c agent-browser -n '__agent_browser_using_command console' -l clear -d 'Clear console logs'
complete -c agent-browser -n '__agent_browser_using_command errors'  -l clear -d 'Clear page errors'

# batch
complete -c agent-browser -n '__agent_browser_using_command batch' -l bail -d 'Stop on first error'

# screenshot
complete -c agent-browser -n '__agent_browser_using_command screenshot' -l full     -d 'Full page screenshot'
complete -c agent-browser -n '__agent_browser_using_command screenshot' -l annotate -d 'Annotated screenshot'

# ── global options ──

complete -c agent-browser -l session           -d 'Isolated session name' -x
complete -c agent-browser -l executable-path   -d 'Custom browser executable' -rF
complete -c agent-browser -l extension         -d 'Load browser extension' -rF
complete -c agent-browser -l args              -d 'Browser launch args' -x
complete -c agent-browser -l user-agent        -d 'Custom User-Agent' -x
complete -c agent-browser -l proxy             -d 'Proxy server URL' -x
complete -c agent-browser -l proxy-bypass      -d 'Bypass proxy for hosts' -x
complete -c agent-browser -l ignore-https-errors -d 'Ignore HTTPS errors'
complete -c agent-browser -l allow-file-access -d 'Allow file:// URLs'
complete -c agent-browser -s p -l provider     -d 'Browser provider' -xa 'ios browserbase kernel browseruse browserless agentcore'
complete -c agent-browser -l device            -d 'iOS device name' -x
complete -c agent-browser -l json              -d 'JSON output'
complete -c agent-browser -l annotate          -d 'Annotated screenshot'
complete -c agent-browser -l screenshot-dir    -d 'Screenshot directory' -rF
complete -c agent-browser -l screenshot-quality -d 'JPEG quality 0-100' -x
complete -c agent-browser -l screenshot-format -d 'Screenshot format' -xa 'png jpeg'
complete -c agent-browser -l headed            -d 'Show browser window'
complete -c agent-browser -l cdp               -d 'Connect via CDP port' -x
complete -c agent-browser -l color-scheme      -d 'Color scheme' -xa 'dark light no-preference'
complete -c agent-browser -l download-path     -d 'Download directory' -rF
complete -c agent-browser -l content-boundaries -d 'Wrap output in markers'
complete -c agent-browser -l max-output        -d 'Truncate to N chars' -x
complete -c agent-browser -l allowed-domains   -d 'Restrict domains' -x
complete -c agent-browser -l action-policy     -d 'Action policy file' -rF
complete -c agent-browser -l confirm-actions   -d 'Categories needing confirmation' -x
complete -c agent-browser -l confirm-interactive -d 'Interactive confirmation'
complete -c agent-browser -l engine            -d 'Browser engine' -xa 'chrome lightpanda'
complete -c agent-browser -l no-auto-dialog    -d 'Disable auto dialog dismiss'
complete -c agent-browser -l config            -d 'Config file path' -rF
complete -c agent-browser -l profile           -d 'Persistent login profile' -rF
complete -c agent-browser -l session-name      -d 'Auto-save/restore state name' -x
complete -c agent-browser -l state             -d 'Load saved auth state' -rF
complete -c agent-browser -l auto-connect      -d 'Connect to running Chrome'
complete -c agent-browser -l headers           -d 'HTTP headers (JSON)' -x
complete -c agent-browser -l debug             -d 'Debug output'
complete -c agent-browser -s V -l version      -d 'Show version'
