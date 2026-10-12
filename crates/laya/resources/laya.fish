complete -c laya -f
complete -c laya -n '__fish_use_subcommand' -a 'dashboard status stop snake service mcp version help completion'
complete -c laya -n '__fish_use_subcommand' -l version -d 'Show version'
complete -c laya -n '__fish_use_subcommand' -l snake -d 'Offline demo'
complete -c laya -l help -s h -d 'Show help'
complete -c laya -n '__fish_seen_subcommand_from help' -a 'dashboard status stop snake service mcp version help completion'
complete -c laya -n '__fish_seen_subcommand_from completion' -a 'bash zsh fish'
complete -c laya -n '__fish_seen_subcommand_from dashboard service' -l port -x -d 'Local port; default 18686'
complete -c laya -n '__fish_seen_subcommand_from dashboard' -l no-open -d 'Print URL only'
complete -c laya -n '__fish_seen_subcommand_from snake --snake' -l model -r -a '(__fish_complete_directories)' -d 'Local model directory'
complete -c laya -n '__fish_seen_subcommand_from snake --snake' -l record -r -F -d 'Write decisions as JSONL'
complete -c laya -n '__fish_seen_subcommand_from snake --snake' -l prompt -x -a 'compact detailed'
for option in width height seed initial-length fps duration steps
    complete -c laya -n '__fish_seen_subcommand_from snake --snake' -l $option -x
end
for option in optimize max-speed unassisted headless no-alt-screen
    complete -c laya -n '__fish_seen_subcommand_from snake --snake' -l $option
end
