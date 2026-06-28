pub(crate) const INIT_TEMPLATE: &str = include_str!("../init_template.toml");

pub(crate) enum CliMode {
    Run,
    Validate,
    Help,
    Init(Option<String>),
    Quickstart,
}

pub(crate) struct CliResult {
    pub mode: CliMode,
    pub force: bool,
}

pub(crate) fn parse_args() -> CliResult {
    let args: Vec<String> = std::env::args().collect();
    let mut mode = CliMode::Run;
    let mut force = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--validate" => {
                mode = CliMode::Validate;
                i += 1;
            }
            "--help" => {
                mode = CliMode::Help;
                i += 1;
            }
            "--init" => {
                let mut j = i + 1;
                if args.get(j).map(|a| a.as_str()) == Some("--force") {
                    force = true;
                    j += 1;
                }
                let path = match args.get(j) {
                    Some(s) if s.starts_with("--") => {
                        eprintln!("unknown argument: {s}");
                        std::process::exit(2);
                    }
                    Some(s) => {
                        j += 1;
                        Some(s.clone())
                    }
                    None => None,
                };
                if args.get(j).map(|a| a.as_str()) == Some("--force") {
                    force = true;
                    j += 1;
                }
                i = j;
                mode = CliMode::Init(path);
            }
            "--quickstart" => {
                mode = CliMode::Quickstart;
                i += 1;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            _ => {
                eprintln!("unknown argument: {}", args[i]);
                std::process::exit(2);
            }
        }
    }
    CliResult { mode, force }
}

pub(crate) fn print_help() {
    print!(
        "\
frugalis — intent-aware routing gateway

USAGE:
    frugalis [OPTIONS]

OPTIONS:
    --help         Show this help
    --init [PATH]  Generate a starter config (default: stdout)
    --force        With --init, overwrite an existing file at PATH
    --quickstart   Interactive setup wizard
    --validate     Validate configuration and exit

ENVIRONMENT:
    CONFIG_PATH              Path to config overlay (TOML or YAML)
    PROXY_API_BEARER_TOKEN   Required for proxy routes
    DASHBOARD_BASIC_USER     Required for dashboard access
    DASHBOARD_BASIC_PASSWORD Required for dashboard access
"
    );
}

pub(crate) fn run_init(path: Option<&str>, force: bool) -> Result<(), String> {
    match path {
        Some(p) => {
            if p.starts_with('-') {
                return Err(format!(
                    "refusing path that starts with '-': {p} (looks like a flag, not a path)"
                ));
            }
            let path = std::path::Path::new(p);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!("failed to create parent directory for {}: {}", p, e)
                    })?;
                }
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .create_new(!force)
                .truncate(force)
                .open(path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        format!(
                            "refusing to overwrite existing file: {p} (use --force to overwrite)"
                        )
                    } else {
                        format!("failed to write {p}: {e}")
                    }
                })?;
            std::io::Write::write_all(&mut file, INIT_TEMPLATE.as_bytes())
                .map_err(|e| format!("failed to write {p}: {e}"))?;
            eprintln!("Wrote starter config to {p}");
        }
        None => {
            print!("{}", INIT_TEMPLATE);
        }
    }
    Ok(())
}
