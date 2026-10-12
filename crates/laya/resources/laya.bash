_laya() {
    local current="${COMP_WORDS[COMP_CWORD]}" command="${COMP_WORDS[1]}" previous="${COMP_WORDS[COMP_CWORD-1]}" words="" i
    COMPREPLY=()
    for ((i=1; i<COMP_CWORD; i++)); do [[ "${COMP_WORDS[i]}" == -- ]] && return; done
    if ((COMP_CWORD == 1)) || [[ "$command" == help && $COMP_CWORD == 2 ]]; then
        words='dashboard status stop snake service mcp version help completion --help --version --snake'
    elif [[ "$command" == completion && $COMP_CWORD == 2 ]]; then
        words='bash zsh fish'
    elif [[ "$command" == dashboard || "$command" == service ]]; then
        [[ "$previous" == --port || "$current" == --port=* ]] && return
        words='--help --port'
        [[ "$command" == dashboard ]] && words="$words --no-open"
        for ((i=2; i<COMP_CWORD; i++)); do
            case "${COMP_WORDS[i]}" in
                --port|--port=*) words="${words/--port/}" ;;
                --no-open) words="${words/--no-open/}" ;;
            esac
        done
    elif [[ "$command" == snake || "$command" == --snake ]]; then
        case "$previous" in
            --prompt) words='compact detailed' ;;
            --model|--record)
                local item kind=-f
                [[ "$previous" == --model ]] && kind=-d
                while IFS= read -r item; do COMPREPLY+=("$item"); done < <(compgen "$kind" -- "$current")
                return ;;
            --width|--height|--seed|--initial-length|--fps|--duration|--steps) return ;;
            *) words='--help --model --prompt --optimize --width --height --seed --initial-length --fps --max-speed --duration --steps --unassisted --record --headless --no-alt-screen' ;;
        esac
    else
        words='--help'
    fi
    COMPREPLY=( $(compgen -W "$words" -- "$current") )
}
complete -F _laya laya
