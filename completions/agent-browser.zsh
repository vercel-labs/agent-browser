#compdef agent-browser

# Zsh completion for agent-browser
# https://github.com/vercel-labs/agent-browser

_agent_browser_global_options() {
  local -a opts=(
    '--session[Isolated session name]:name:'
    '--executable-path[Custom browser executable]:path:_files'
    '--extension[Load browser extension]:path:_files -/'
    '--args[Browser launch args]:args:'
    '--user-agent[Custom User-Agent]:ua:'
    '--proxy[Proxy server URL]:url:'
    '--proxy-bypass[Bypass proxy for hosts]:hosts:'
    '--ignore-https-errors[Ignore HTTPS certificate errors]'
    '--allow-file-access[Allow file:// URLs]'
    '(-p --provider)'{-p,--provider}'[Browser provider]:provider:(ios browserbase kernel browseruse browserless agentcore)'
    '--device[iOS device name]:name:'
    '--json[JSON output]'
    '--annotate[Annotated screenshot with labels]'
    '--screenshot-dir[Default screenshot directory]:path:_files -/'
    '--screenshot-quality[JPEG quality 0-100]:quality:'
    '--screenshot-format[Screenshot format]:format:(png jpeg)'
    '--headed[Show browser window]'
    '--cdp[Connect via CDP port]:port:'
    '--color-scheme[Color scheme]:scheme:(dark light no-preference)'
    '--download-path[Default download directory]:path:_files -/'
    '--content-boundaries[Wrap output in boundary markers]'
    '--max-output[Truncate output to N chars]:chars:'
    '--allowed-domains[Restrict navigation domains]:list:'
    '--action-policy[Action policy JSON file]:path:_files'
    '--confirm-actions[Categories requiring confirmation]:list:'
    '--confirm-interactive[Interactive confirmation prompts]'
    '--engine[Browser engine]:engine:(chrome lightpanda)'
    '--no-auto-dialog[Disable automatic dialog dismissal]'
    '--config[Custom config file]:path:_files'
    '--profile[Persist login sessions]:path:_files -/'
    '--session-name[Auto-save/restore state name]:name:'
    '--state[Load saved auth state]:path:_files'
    '--auto-connect[Connect to running Chrome]'
    '--headers[HTTP headers (JSON)]:json:'
    '--debug[Debug output]'
    '(-V --version)'{-V,--version}'[Show version]'
  )
  _values -w 'global options' $opts
}

_agent_browser_snapshot_options() {
  local -a opts=(
    '(-i --interactive)'{-i,--interactive}'[Only interactive elements]'
    '(-c --compact)'{-c,--compact}'[Remove empty structural elements]'
    '(-d --depth)'{-d,--depth}'[Limit tree depth]:depth:'
    '(-s --selector)'{-s,--selector}'[Scope to CSS selector]:selector:'
  )
  _arguments $opts
}

_agent_browser_get_subcommands() {
  local -a subcmds=(
    'text:Get text content'
    'html:Get HTML content'
    'value:Get input value'
    'attr:Get attribute value'
    'title:Get page title'
    'url:Get page URL'
    'count:Get element count'
    'box:Get bounding box'
    'styles:Get computed styles'
    'cdp-url:Get CDP WebSocket URL'
  )
  _describe 'get subcommand' subcmds
}

_agent_browser_is_subcommands() {
  local -a subcmds=(
    'visible:Check if element is visible'
    'enabled:Check if element is enabled'
    'checked:Check if element is checked'
  )
  _describe 'is subcommand' subcmds
}

_agent_browser_find_subcommands() {
  local -a subcmds=(
    'role:Find by ARIA role'
    'text:Find by text content'
    'label:Find by label'
    'placeholder:Find by placeholder'
    'alt:Find by alt text'
    'title:Find by title'
    'testid:Find by test ID'
    'first:Find first match'
    'last:Find last match'
    'nth:Find nth match'
  )
  _describe 'find subcommand' subcmds
}

_agent_browser_mouse_subcommands() {
  local -a subcmds=(
    'move:Move mouse to coordinates'
    'down:Press mouse button'
    'up:Release mouse button'
    'wheel:Scroll mouse wheel'
  )
  _describe 'mouse subcommand' subcmds
}

_agent_browser_set_subcommands() {
  local -a subcmds=(
    'viewport:Set viewport size'
    'device:Set device emulation'
    'geo:Set geolocation'
    'offline:Set offline mode'
    'headers:Set HTTP headers'
    'credentials:Set HTTP credentials'
    'media:Set media features'
  )
  _describe 'set subcommand' subcmds
}

_agent_browser_network_subcommands() {
  local -a subcmds=(
    'route:Intercept network requests'
    'unroute:Remove network interception'
    'requests:View captured requests'
    'har:Record HAR file'
  )
  _describe 'network subcommand' subcmds
}

_agent_browser_storage_subcommands() {
  local -a subcmds=(
    'local:Manage localStorage'
    'session:Manage sessionStorage'
  )
  _describe 'storage subcommand' subcmds
}

_agent_browser_tab_subcommands() {
  local -a subcmds=(
    'new:Open new tab'
    'list:List tabs'
    'close:Close tab'
  )
  _describe 'tab subcommand' subcmds
}

_agent_browser_cookies_subcommands() {
  local -a subcmds=(
    'get:Get cookies'
    'set:Set cookie'
    'clear:Clear cookies'
  )
  _describe 'cookies subcommand' subcmds
}

_agent_browser_auth_subcommands() {
  local -a subcmds=(
    'save:Save auth profile'
    'login:Login using saved credentials'
    'list:List saved auth profiles'
    'show:Show auth profile metadata'
    'delete:Delete auth profile'
  )
  _describe 'auth subcommand' subcmds
}

_agent_browser_session_subcommands() {
  local -a subcmds=(
    'list:List active sessions'
  )
  _describe 'session subcommand' subcmds
}

_agent_browser_dashboard_subcommands() {
  local -a subcmds=(
    'start:Start the dashboard server'
    'stop:Stop the dashboard server'
    'install:Install the dashboard'
  )
  _describe 'dashboard subcommand' subcmds
}

_agent_browser_stream_subcommands() {
  local -a subcmds=(
    'enable:Start WebSocket streaming'
    'disable:Stop WebSocket streaming'
    'status:Show streaming status'
  )
  _describe 'stream subcommand' subcmds
}

_agent_browser_diff_subcommands() {
  local -a subcmds=(
    'snapshot:Compare current vs last snapshot'
    'screenshot:Compare current vs baseline image'
    'url:Compare two pages'
  )
  _describe 'diff subcommand' subcmds
}

_agent_browser_state_subcommands() {
  local -a subcmds=(
    'save:Save browser state'
    'load:Load browser state'
    'list:List saved states'
    'clear:Clear saved states'
    'show:Show state details'
    'clean:Clean expired states'
    'rename:Rename saved state'
  )
  _describe 'state subcommand' subcmds
}

_agent_browser_trace_subcommands() {
  local -a subcmds=(
    'start:Start recording trace'
    'stop:Stop and save trace'
  )
  _describe 'trace subcommand' subcmds
}

_agent_browser_profiler_subcommands() {
  local -a subcmds=(
    'start:Start profiling'
    'stop:Stop and save profile'
  )
  _describe 'profiler subcommand' subcmds
}

_agent_browser_record_subcommands() {
  local -a subcmds=(
    'start:Start video recording'
    'stop:Stop and save video'
    'restart:Restart recording'
  )
  _describe 'record subcommand' subcmds
}

_agent_browser_clipboard_subcommands() {
  local -a subcmds=(
    'read:Read clipboard'
    'write:Write to clipboard'
    'copy:Copy to clipboard'
    'paste:Paste from clipboard'
  )
  _describe 'clipboard subcommand' subcmds
}

_agent_browser_device_subcommands() {
  local -a subcmds=(
    'list:List iOS simulators'
  )
  _describe 'device subcommand' subcmds
}

_agent_browser_dialog_subcommands() {
  local -a subcmds=(
    'accept:Accept dialog'
    'dismiss:Dismiss dialog'
    'status:Show dialog status'
  )
  _describe 'dialog subcommand' subcmds
}

_agent_browser_scroll_directions() {
  local -a dirs=(
    'up:Scroll up'
    'down:Scroll down'
    'left:Scroll left'
    'right:Scroll right'
  )
  _describe 'direction' dirs
}

_agent_browser_wait_options() {
  local -a opts=(
    '--url[Wait for URL pattern]:pattern:'
    '--load[Wait for load state]:state:(load domcontentloaded networkidle)'
    '--fn[Wait for JS function]:function:'
    '--text[Wait for text content]:text:'
    '--download[Wait for download]'
  )
  _arguments $opts
}

_agent_browser() {
  local curcontext="$curcontext" state line
  typeset -A opt_args

  local -a commands=(
    # core
    'open:Navigate to URL'
    'click:Click element'
    'dblclick:Double-click element'
    'type:Type into element'
    'fill:Clear and fill element'
    'press:Press key'
    'keyboard:Keyboard actions'
    'hover:Hover element'
    'focus:Focus element'
    'check:Check checkbox'
    'uncheck:Uncheck checkbox'
    'select:Select dropdown option'
    'drag:Drag and drop'
    'upload:Upload files'
    'download:Download file by clicking element'
    'scroll:Scroll page'
    'scrollintoview:Scroll element into view'
    'wait:Wait for element or time'
    'screenshot:Take screenshot'
    'pdf:Save as PDF'
    'snapshot:Accessibility tree with refs'
    'eval:Run JavaScript'
    'connect:Connect via CDP'
    'close:Close browser'
    # navigation
    'back:Go back'
    'forward:Go forward'
    'reload:Reload page'
    # compound
    'get:Get element info'
    'is:Check element state'
    'find:Find elements by locator'
    'mouse:Mouse actions'
    'set:Browser settings'
    'network:Network actions'
    'storage:Manage web storage'
    'cookies:Manage cookies'
    'tab:Manage tabs'
    'auth:Auth vault'
    'session:Session management'
    'dashboard:Dashboard server'
    'stream:WebSocket streaming'
    'diff:Compare pages/snapshots'
    'state:Manage browser state'
    # debug
    'trace:Record Chrome DevTools trace'
    'profiler:Record Chrome DevTools profile'
    'record:Video recording'
    'console:View console logs'
    'errors:View page errors'
    'highlight:Highlight element'
    'inspect:Open Chrome DevTools'
    'clipboard:Clipboard operations'
    # confirmation
    'confirm:Approve pending action'
    'deny:Deny pending action'
    # iOS
    'tap:Touch element (iOS)'
    'swipe:Swipe gesture (iOS)'
    'device:iOS device management'
    # dialog
    'dialog:Dialog management'
    # frame
    'frame:Switch frame context'
    # window
    'window:Window management'
    # batch
    'batch:Execute commands from stdin'
    # setup
    'install:Install browser binaries'
    'upgrade:Upgrade to latest version'
  )

  _arguments -C \
    '1:command:->command' \
    '*::arg:->args' \
    && return

  case $state in
    command)
      _describe 'command' commands
      ;;
    args)
      case ${line[1]} in
        get)        _agent_browser_get_subcommands ;;
        is)         _agent_browser_is_subcommands ;;
        find)       _agent_browser_find_subcommands ;;
        mouse)      _agent_browser_mouse_subcommands ;;
        set)        _agent_browser_set_subcommands ;;
        network)    _agent_browser_network_subcommands ;;
        storage)    _agent_browser_storage_subcommands ;;
        tab)        _agent_browser_tab_subcommands ;;
        cookies)    _agent_browser_cookies_subcommands ;;
        auth)       _agent_browser_auth_subcommands ;;
        session)    _agent_browser_session_subcommands ;;
        dashboard)  _agent_browser_dashboard_subcommands ;;
        stream)     _agent_browser_stream_subcommands ;;
        diff)       _agent_browser_diff_subcommands ;;
        state)      _agent_browser_state_subcommands ;;
        trace)      _agent_browser_trace_subcommands ;;
        profiler)   _agent_browser_profiler_subcommands ;;
        record)     _agent_browser_record_subcommands ;;
        clipboard)  _agent_browser_clipboard_subcommands ;;
        device)     _agent_browser_device_subcommands ;;
        dialog)     _agent_browser_dialog_subcommands ;;
        scroll)     _agent_browser_scroll_directions ;;
        wait)       _agent_browser_wait_options ;;
        snapshot)   _agent_browser_snapshot_options ;;
        screenshot) _arguments '--full[Full page screenshot]' '1:path:_files' ;;
        pdf)        _arguments '1:path:_files' ;;
        open)       _arguments '1:url:' ;;
        connect)    _arguments '1:port or url:' ;;
        close)      _arguments '--all[Close all sessions]' ;;
        install)    _arguments '--with-deps[Install system dependencies]' ;;
        console)    _arguments '--clear[Clear console logs]' ;;
        errors)     _arguments '--clear[Clear page errors]' ;;
        batch)      _arguments '--bail[Stop on first error]' ;;
        *)          _default ;;
      esac
      ;;
  esac
}

_agent_browser "$@"
