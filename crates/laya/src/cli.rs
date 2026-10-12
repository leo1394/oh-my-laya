//! Dependency-free discovery commands; never start services or load a model.
use anyhow::{bail,Result};

pub const COMMANDS:&[&str]=&["dashboard","status","stop","snake","service","mcp","version","help","completion"];
pub const REPOSITORY:&str="https://github.com/leo1394/oh-my-laya";
pub const RELEASE_DATE:&str=include_str!("../../../RELEASE_DATE.txt");

pub fn version()->String {
    let suffix=if option_env!("LAYA_RELEASE_BUILD")==Some("1") {""} else {"-dev"};
    format!("laya version {}{suffix} ({})\n{REPOSITORY}\n",env!("CARGO_PKG_VERSION"),RELEASE_DATE.trim())
}

pub fn unknown(kind:&str,value:&str,choices:&[&str],topic:&str)->anyhow::Error {
    let mut candidates=choices.iter().filter_map(|candidate| {
        let distance=distance(value,candidate);
        (distance<=2 && distance*2<=value.len().max(candidate.len())).then_some((distance,*candidate))
    }).collect::<Vec<_>>();
    candidates.sort();
    let suggestion=candidates.first().map(|(_,candidate)|format!("\nDid you mean '{candidate}'?")).unwrap_or_default();
    anyhow::anyhow!("unknown {kind}: {value}{suggestion}\nRun 'laya help{topic}' for usage.")
}

fn distance(left:&str,right:&str)->usize {
    let mut row=(0..=right.chars().count()).collect::<Vec<_>>();
    for (i,a) in left.chars().enumerate() {
        let mut previous=row[0];row[0]=i+1;
        for (j,b) in right.chars().enumerate() {
            let old=row[j+1];row[j+1]=(row[j]+1).min(old+1).min(previous+usize::from(a!=b));previous=old;
        }
    }
    *row.last().unwrap()
}

pub fn help(topic:Option<&str>)->Result<String> {
    let text=match topic {
        None=>"Oh My Laya — native local AI decisions, routing, and learning.\n\nUsage: laya <command> [options]\n       laya help [command]\n\nCommands:\n  dashboard   Open the local decision workbench\n  status      Show service, collection, and resource status\n  stop        Stop the local service\n  snake       Play the offline Laya demo (alias: --snake)\n  version     Show version, release date, and repository\n  completion  Generate Bash, Zsh, or Fish completion\n  help        Show command help\n  mcp         Run the agent MCP stdio bridge\n  service     Run the local service in the foreground\n\nExamples:\n  laya dashboard\n  laya dashboard --no-open\n  laya help dashboard\n  laya version\n\nRun 'laya help <command>' or 'laya <command> --help' for details.\n",
        Some("dashboard")=>"Open the local decision workbench.\nUsage: laya dashboard [--port PORT] [--no-open]\n\n--port PORT  Listen on 0..65535; default 18686 or LAYA_PORT. 0 selects a temporary port.\n--no-open    Print the pairing URL without opening a browser.\n\nExamples:\n  laya dashboard\n  laya dashboard --port 18686 --no-open\n\nStarts a local service if needed. Requires an installed Python runtime for inference.\nTreat pairing URLs as private. Does not enable data collection automatically.\n",
        Some("status")=>"Show local service, recording, native usage sync, and resource status as JSON.\nUsage: laya status\nExample: laya status\nDoes not start the service or load a model.\n",
        Some("stop")=>"Stop the running local service.\nUsage: laya stop\nExample: laya stop\nRequires a running service; stops local work but preserves recorded data.\n",
        Some("service")=>"Run the local service in the foreground.\nUsage: laya service [--port PORT]\nExample: laya service --port 18686\nDefault port: 18686 or LAYA_PORT; 0 selects a temporary port.\nUses the installed Python runtime for inference. Collection follows saved consent.\n",
        Some("mcp")=>"Run the native Laya MCP stdio bridge.\nUsage: laya mcp\nExample: laya mcp\nFor agent hosts, not interactive output. Starts the local service on demand.\nRequires the installed runtime and model for inference.\n",
        Some("snake"|"--snake")=>"Play the local offline Laya snake demo.\nUsage: laya snake [demo-options]\n       laya --snake [demo-options]\nExample: laya snake\nRequires the installed laya-snake demo and local model weights.\nStops an idle workbench service and acquires the model lock; refuses while busy.\nDemo options are forwarded unchanged. Use 'laya-snake --help' for upstream options.\n",
        Some("version")=>"Show the version, release date, and repository on two lines.\nUsage: laya version | laya --version\nExample: laya version\nDevelopment binaries are marked -dev. No runtime dependencies required.\n",
        Some("completion")=>"Generate shell completion without starting services.\nUsage: laya completion bash|zsh|fish\nExamples:\n  laya completion bash > ~/.local/share/bash-completion/completions/laya\n  laya completion zsh > ~/.local/share/zsh/site-functions/_laya\n  laya completion fish > ~/.config/fish/completions/laya.fish\n\nCreate the destination directory first. Bash: source the file. Zsh: add its directory\nto fpath before compinit. Fish loads ~/.config/fish/completions automatically.\n",
        Some("help")=>"Show help without starting services or loading a model.\nUsage: laya help [command]\nExample: laya help dashboard\nAliases: laya --help; laya <command> --help.\n",
        Some(other)=>return Err(unknown("command",other,COMMANDS,"")),
    };
    let mut text=text.to_string();
    if matches!(topic,Some("snake"|"--snake")) {
        text.push_str("\nDemo options (installed laya-mlx 0.2.0):\n  --model PATH       Override local model directory or cached Hub ID\n  --prompt STYLE     compact (default) or detailed\n  --optimize         Enable model compilation\n  --width N          Board width (24)\n  --height N         Board height (16)\n  --seed N           Random seed (7)\n  --initial-length N Initial length (6)\n  --fps N            Decisions per second (12)\n  --max-speed        Run without pacing\n  --duration N       Stop after N seconds, excluding warmup\n  --steps N          Stop after N steps\n  --unassisted       Disable safety shield\n  --record FILE      Write decisions and board states to JSONL\n  --headless         Run without a terminal display\n  --no-alt-screen    Keep the final frame in scrollback\n");
    }
    Ok(text)
}

pub fn completion(shell:&str)->Result<&'static str> {
    match shell {
        "bash"=>Ok(include_str!("../resources/laya.bash")),
        "zsh"=>Ok(include_str!("../resources/_laya")),
        "fish"=>Ok(include_str!("../resources/laya.fish")),
        other=>Err(unknown("shell",other,&["bash","zsh","fish"]," completion")),
    }
}

pub fn discovery(args:&[String])->Result<bool> {
    let command=args.first().map(String::as_str).unwrap_or("help");
    if command=="help" || command=="--help" || command=="-h" {
        if args.len()>2 || command!="help" && args.len()>1 {bail!("unexpected help arguments; run 'laya help [command]'");}
        print!("{}",help(args.get(1).map(String::as_str))?);return Ok(true);
    }
    if args.len()==2 && matches!(args[1].as_str(),"--help"|"-h") {print!("{}",help(Some(command))?);return Ok(true);}
    if matches!(command,"--version"|"version") {
        if args.len()!=1 {bail!("version accepts no arguments; run 'laya help version'");}
        print!("{}",version());return Ok(true);
    }
    if matches!(command,"completion"|"__completion") {
        if args.len()!=2 {bail!("expected one shell; run 'laya help completion'");}
        print!("{}",completion(&args[1])?);return Ok(true);
    }
    if command=="__manpage" {
        if args.len()!=1 {bail!("__manpage accepts no arguments");}
        print!("{}",include_str!("../resources/laya.1").replace("@VERSION@",env!("CARGO_PKG_VERSION")).replace("@DATE@",RELEASE_DATE.trim()));return Ok(true);
    }
    if matches!(command,"status"|"stop"|"mcp") && args.len()>1 {return Err(unknown("option",&args[1],&["--help"],&format!(" {command}")));}
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_help_and_suggestions() {
        let text=version();assert_eq!(text.lines().count(),2);assert!(text.ends_with('\n'));assert!(text.contains(REPOSITORY));
        for command in COMMANDS {assert!(help(Some(command)).unwrap().contains("Usage:"));}
        assert!(unknown("command","versoin",COMMANDS,"").to_string().contains("'version'"));
        assert!(!unknown("command","unrelated-thing",COMMANDS,"").to_string().contains("Did you mean"));
    }
}
