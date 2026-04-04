# Bash completion for agent-browser
# https://github.com/vercel-labs/agent-browser

_agent_browser() {
  local cur prev words cword
  _init_completion || return

  local commands="open click dblclick type fill press keyboard hover focus check
    uncheck select drag upload download scroll scrollintoview wait screenshot pdf
    snapshot eval connect close back forward reload get is find mouse set network
    storage cookies tab auth session dashboard stream diff state trace profiler
    record console errors highlight inspect clipboard confirm deny tap swipe
    device dialog frame window batch install upgrade"

  local global_opts="--session --executable-path --extension --args --user-agent
    --proxy --proxy-bypass --ignore-https-errors --allow-file-access -p --provider
    --device --json --annotate --screenshot-dir --screenshot-quality
    --screenshot-format --headed --cdp --color-scheme --download-path
    --content-boundaries --max-output --allowed-domains --action-policy
    --confirm-actions --confirm-interactive --engine --no-auto-dialog --config
    --profile --session-name --state --auto-connect --headers --debug -V --version"

  # if completing the first argument, show commands
  if [[ $cword -eq 1 ]]; then
    COMPREPLY=($(compgen -W "$commands" -- "$cur"))
    return
  fi

  local cmd="${words[1]}"

  case "$cmd" in
    get)
      COMPREPLY=($(compgen -W "text html value attr title url count box styles cdp-url" -- "$cur"))
      ;;
    is)
      COMPREPLY=($(compgen -W "visible enabled checked" -- "$cur"))
      ;;
    find)
      COMPREPLY=($(compgen -W "role text label placeholder alt title testid first last nth" -- "$cur"))
      ;;
    mouse)
      COMPREPLY=($(compgen -W "move down up wheel" -- "$cur"))
      ;;
    set)
      COMPREPLY=($(compgen -W "viewport device geo offline headers credentials media" -- "$cur"))
      ;;
    network)
      COMPREPLY=($(compgen -W "route unroute requests har" -- "$cur"))
      ;;
    storage)
      COMPREPLY=($(compgen -W "local session" -- "$cur"))
      ;;
    tab)
      COMPREPLY=($(compgen -W "new list close" -- "$cur"))
      ;;
    cookies)
      COMPREPLY=($(compgen -W "get set clear" -- "$cur"))
      ;;
    auth)
      COMPREPLY=($(compgen -W "save login list show delete" -- "$cur"))
      ;;
    session)
      COMPREPLY=($(compgen -W "list" -- "$cur"))
      ;;
    dashboard)
      COMPREPLY=($(compgen -W "start stop install" -- "$cur"))
      ;;
    stream)
      COMPREPLY=($(compgen -W "enable disable status" -- "$cur"))
      ;;
    diff)
      COMPREPLY=($(compgen -W "snapshot screenshot url" -- "$cur"))
      ;;
    state)
      COMPREPLY=($(compgen -W "save load list clear show clean rename" -- "$cur"))
      ;;
    trace)
      COMPREPLY=($(compgen -W "start stop" -- "$cur"))
      ;;
    profiler)
      COMPREPLY=($(compgen -W "start stop" -- "$cur"))
      ;;
    record)
      COMPREPLY=($(compgen -W "start stop restart" -- "$cur"))
      ;;
    clipboard)
      COMPREPLY=($(compgen -W "read write copy paste" -- "$cur"))
      ;;
    device)
      COMPREPLY=($(compgen -W "list" -- "$cur"))
      ;;
    dialog)
      COMPREPLY=($(compgen -W "accept dismiss status" -- "$cur"))
      ;;
    scroll)
      COMPREPLY=($(compgen -W "up down left right" -- "$cur"))
      ;;
    snapshot)
      COMPREPLY=($(compgen -W "-i --interactive -c --compact -d --depth -s --selector" -- "$cur"))
      ;;
    wait)
      COMPREPLY=($(compgen -W "--url --load --fn --text --download" -- "$cur"))
      ;;
    close)
      COMPREPLY=($(compgen -W "--all" -- "$cur"))
      ;;
    install)
      COMPREPLY=($(compgen -W "--with-deps" -- "$cur"))
      ;;
    console|errors)
      COMPREPLY=($(compgen -W "--clear" -- "$cur"))
      ;;
    batch)
      COMPREPLY=($(compgen -W "--bail" -- "$cur"))
      ;;
    screenshot)
      COMPREPLY=($(compgen -W "--full --annotate" -- "$cur"))
      ;;
    keyboard)
      COMPREPLY=($(compgen -W "type inserttext" -- "$cur"))
      ;;
    window)
      COMPREPLY=($(compgen -W "new" -- "$cur"))
      ;;
    har)
      COMPREPLY=($(compgen -W "start stop" -- "$cur"))
      ;;
    *)
      # for unknown subcommands, offer global options
      if [[ "$cur" == -* ]]; then
        COMPREPLY=($(compgen -W "$global_opts" -- "$cur"))
      fi
      ;;
  esac
}

complete -F _agent_browser agent-browser
