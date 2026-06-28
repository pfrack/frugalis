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

#[cfg(test)]
mod tests {
    use super::*;

    fn init_scratch(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let dir = std::env::temp_dir().join(format!("frugalis-init-{label}-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir should be creatable");
        dir
    }

    #[test]
    fn init_template_contains_all_five_routing_sections() {
        for section in ["[routing.DEFAULT]", "[routing.FILE_READING]", "[routing.SYNTAX_FIX]", "[routing.COMPLEX_REASONING]", "[routing.CASUAL]"] {
            assert!(INIT_TEMPLATE.contains(section), "init template should contain section {section}");
        }
    }

    #[test]
    fn init_template_parses_as_valid_toml_syntax() {
        let value: toml::Value = toml::from_str(INIT_TEMPLATE).expect("init template should be valid TOML syntax");
        let table = value.as_table().expect("init template should be a top-level TOML table");
        let routing = table.get("routing").and_then(|v| v.as_table()).expect("init template should have a [routing] table");
        assert_eq!(routing.len(), 5);
    }

    #[test]
    fn run_init_writes_template_to_new_file() {
        let dir = init_scratch("write");
        let path = dir.join("frugalis.toml");
        run_init(Some(path.to_str().unwrap()), false).expect("write should succeed");
        let content = std::fs::read_to_string(&path).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }

    #[test]
    fn run_init_refuses_to_overwrite_existing_file() {
        let dir = init_scratch("refuse");
        let path = dir.join("frugalis.toml");
        std::fs::write(&path, "preexisting content").expect("seed write should succeed");
        let err = run_init(Some(path.to_str().unwrap()), false).expect_err("overwrite must be refused without --force");
        assert!(err.contains("refusing to overwrite"));
        let still = std::fs::read_to_string(&path).expect("file should still be readable");
        assert_eq!(still, "preexisting content");
    }

    #[test]
    fn run_init_force_overwrites_existing_file() {
        let dir = init_scratch("force");
        let path = dir.join("frugalis.toml");
        std::fs::write(&path, "preexisting content").expect("seed write should succeed");
        run_init(Some(path.to_str().unwrap()), true).expect("force overwrite should succeed");
        let content = std::fs::read_to_string(&path).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }

    #[test]
    fn run_init_creates_missing_parent_directories() {
        let dir = init_scratch("mkdir");
        let nested = dir.join("a").join("b").join("frugalis.toml");
        run_init(Some(nested.to_str().unwrap()), false).expect("nested write should succeed");
        assert!(nested.exists());
        let content = std::fs::read_to_string(&nested).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }
}
