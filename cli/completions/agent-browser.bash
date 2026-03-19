# Shell completion for agent-browser
# Source: cli/completions/agent-browser.bash
# Keep in sync with: cli/src/commands.rs and cli/src/flags.rs
#
# To activate:
#   eval "$(agent-browser completion bash)"
#   # or add that line to ~/.bashrc

_agent_browser() {
    local cur prev
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    local global_flags="--json --headed --debug --session --headers --executable-path --cdp --extension --profile --state --proxy --proxy-bypass --args --user-agent --provider -p --device --ignore-https-errors --allow-file-access --auto-connect --session-name --annotate --color-scheme --download-path --content-boundaries --max-output --allowed-domains --action-policy --confirm-actions --confirm-interactive --engine --screenshot-dir --screenshot-quality --screenshot-format --idle-timeout --config"

    # Flags that consume the next token as a value.
    # Keep in sync with GLOBAL_FLAGS_WITH_VALUE in flags.rs.
    local flags_with_value="--session --headers --executable-path --cdp --extension --profile --state --proxy --proxy-bypass --args --user-agent --provider -p --device --session-name --color-scheme --download-path --max-output --allowed-domains --action-policy --confirm-actions --config --engine --screenshot-dir --screenshot-quality --screenshot-format --idle-timeout"

    # Complete values for flags that take a fixed set of options.
    case "$prev" in
        --provider|-p)
            COMPREPLY=( $(compgen -W "chromium firefox webkit ios android" -- "$cur") )
            return 0 ;;
        --engine)
            COMPREPLY=( $(compgen -W "chrome lightpanda" -- "$cur") )
            return 0 ;;
        --color-scheme)
            COMPREPLY=( $(compgen -W "light dark" -- "$cur") )
            return 0 ;;
        --screenshot-format)
            COMPREPLY=( $(compgen -W "png jpeg" -- "$cur") )
            return 0 ;;
        --wait-until|--load|-l)
            COMPREPLY=( $(compgen -W "load domcontentloaded networkidle" -- "$cur") )
            return 0 ;;
        --sameSite)
            COMPREPLY=( $(compgen -W "Strict Lax None" -- "$cur") )
            return 0 ;;
        --executable-path|--profile|--state|--screenshot-dir|--download-path|--config|-b|--baseline|-o|--output)
            COMPREPLY=( $(compgen -f -- "$cur") )
            return 0 ;;
        --extension)
            COMPREPLY=( $(compgen -d -- "$cur") )
            return 0 ;;
    esac

    # If $prev is a value-consuming flag, $cur is its value — nothing to complete.
    if [[ " $flags_with_value " == *" $prev "* ]]; then
        return 0
    fi

    # Find the command word: first non-flag, non-flag-value word after argv[0].
    local cmd="" cmd_pos=0 i word skip_next=0
    for (( i=1; i<COMP_CWORD; i++ )); do
        word="${COMP_WORDS[$i]}"
        if (( skip_next )); then
            skip_next=0
            continue
        fi
        if [[ " $flags_with_value " == *" $word "* ]]; then
            skip_next=1
            continue
        fi
        if [[ "$word" != -* ]]; then
            cmd="$word"
            cmd_pos=$i
            break
        fi
    done

    # No command yet — complete commands or global flags.
    if [[ -z "$cmd" ]]; then
        if [[ "$cur" == -* ]]; then
            COMPREPLY=( $(compgen -W "$global_flags" -- "$cur") )
        else
            local commands="open goto navigate back forward reload click dblclick fill type hover focus check uncheck select drag upload download scroll scrollintoview press key keyboard get is find screenshot pdf snapshot eval connect wait inspect batch cookies storage state network trace profiler record set mouse tab window frame dialog console errors highlight clipboard diff auth close install upgrade session confirm deny completion"
            COMPREPLY=( $(compgen -W "$commands" -- "$cur") )
        fi
        return 0
    fi

    # Find subcommand: first non-flag word after the command.
    local sub="" skip_next2=0
    for (( i=cmd_pos+1; i<COMP_CWORD; i++ )); do
        word="${COMP_WORDS[$i]}"
        if (( skip_next2 )); then
            skip_next2=0
            continue
        fi
        if [[ "$word" == -* ]]; then
            if [[ " $flags_with_value " == *" $word "* ]]; then
                skip_next2=1
            fi
            continue
        fi
        sub="$word"
        break
    done

    # Complete flags at any depth — context-aware using both $cmd and $sub.
    if [[ "$cur" == -* ]]; then
        local cmd_flags=""
        case "$cmd" in
            snapshot)
                cmd_flags="-i --interactive -c --compact -C --cursor -d --depth -s --selector" ;;
            wait)
                cmd_flags="--url -u --load -l --fn -f --text -t --download -d --timeout" ;;
            screenshot)
                cmd_flags="--full -f" ;;
            click|dblclick|hover)
                cmd_flags="--new-tab" ;;
            eval)
                cmd_flags="-b --base64 --stdin" ;;
            scroll|scrollintoview|highlight)
                cmd_flags="-s --selector" ;;
            batch)
                cmd_flags="--bail" ;;
            auth)
                cmd_flags="--url --username --password --password-stdin --username-selector --password-selector --submit-selector" ;;
            cookies)
                cmd_flags="--url --domain --path --httpOnly --secure --sameSite --expires" ;;
            diff)
                case "$sub" in
                    snapshot) cmd_flags="-b --baseline -s --selector -c --compact -d --depth" ;;
                    screenshot) cmd_flags="-b --baseline -s --selector -o --output -t --threshold -f --full" ;;
                    url) cmd_flags="-s --selector -c --compact -d --depth -f --full --screenshot --wait-until" ;;
                    *) cmd_flags="-b --baseline -s --selector -c --compact -d --depth -o --output -t --threshold -f --full --screenshot --wait-until" ;;
                esac ;;
            state)
                cmd_flags="--all -a --older-than" ;;
            profiler)
                cmd_flags="--categories" ;;
            find)
                cmd_flags="--exact --name" ;;
            network)
                case "$sub" in
                    route) cmd_flags="--abort --body" ;;
                esac ;;
        esac
        COMPREPLY=( $(compgen -W "$global_flags $cmd_flags" -- "$cur") )
        return 0
    fi

    # Complete subcommand if we don't have one yet.
    if [[ -z "$sub" ]]; then
        case "$cmd" in
            auth)       COMPREPLY=( $(compgen -W "save login list delete show" -- "$cur") ) ;;
            cookies)    COMPREPLY=( $(compgen -W "get set clear" -- "$cur") ) ;;
            network)    COMPREPLY=( $(compgen -W "route unroute requests har" -- "$cur") ) ;;
            get)        COMPREPLY=( $(compgen -W "text html value attr title url count box styles cdp-url" -- "$cur") ) ;;
            is)         COMPREPLY=( $(compgen -W "visible enabled checked" -- "$cur") ) ;;
            find)       COMPREPLY=( $(compgen -W "role text label placeholder alt title testid first last nth" -- "$cur") ) ;;
            set)        COMPREPLY=( $(compgen -W "viewport device geo geolocation offline headers credentials auth media" -- "$cur") ) ;;
            mouse)      COMPREPLY=( $(compgen -W "move down up wheel" -- "$cur") ) ;;
            tab)        COMPREPLY=( $(compgen -W "new list close" -- "$cur") ) ;;
            keyboard)   COMPREPLY=( $(compgen -W "type inserttext" -- "$cur") ) ;;
            diff)       COMPREPLY=( $(compgen -W "snapshot screenshot url" -- "$cur") ) ;;
            clipboard)  COMPREPLY=( $(compgen -W "read write copy paste" -- "$cur") ) ;;
            scroll)     COMPREPLY=( $(compgen -W "up down left right" -- "$cur") ) ;;
            storage)    COMPREPLY=( $(compgen -W "local session" -- "$cur") ) ;;
            state)      COMPREPLY=( $(compgen -W "save load list clear show clean rename" -- "$cur") ) ;;
            trace|profiler)  COMPREPLY=( $(compgen -W "start stop" -- "$cur") ) ;;
            record)          COMPREPLY=( $(compgen -W "start stop restart" -- "$cur") ) ;;
            dialog)     COMPREPLY=( $(compgen -W "accept dismiss" -- "$cur") ) ;;
            window)     COMPREPLY=( $(compgen -W "new" -- "$cur") ) ;;
            session)    COMPREPLY=( $(compgen -W "list" -- "$cur") ) ;;
            completion) COMPREPLY=( $(compgen -W "bash zsh" -- "$cur") ) ;;
        esac
        return 0
    fi

    # Depth 3: network har start|stop
    if [[ "$cmd" == "network" && "$sub" == "har" ]]; then
        COMPREPLY=( $(compgen -W "start stop" -- "$cur") )
        return 0
    fi
}
complete -F _agent_browser agent-browser
