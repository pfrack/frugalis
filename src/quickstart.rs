//! Interactive setup wizard for `cerebrum --quickstart`.
//!
//! Walks the user through provider selection, model names, and (optionally)
//! an API key env var, then writes a complete routing config — one entry
//! per category (FILE_READING, SYNTAX_FIX, COMPLEX_REASONING, CASUAL,
//! DEFAULT) — to a file the user can point at with `CONFIG_PATH`.
//!
//! Pure data → TOML conversion lives in [`build_quickstart_toml`] so it can
//! be unit-tested without spawning an interactive session.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

const ROUTING_CATEGORIES: &[&str] = &[
    "FILE_READING",
    "SYNTAX_FIX",
    "COMPLEX_REASONING",
    "CASUAL",
    "DEFAULT",
];

struct ProviderPreset {
    label: &'static str,
    endpoint: &'static str,
    provider_type: &'static str,
    api_key_env_default: Option<&'static str>,
}

const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        label: "OpenRouter",
        endpoint: "https://openrouter.ai/api/v1/chat/completions",
        provider_type: "openai_compatible",
        api_key_env_default: Some("OPENROUTER_API_KEY"),
    },
    ProviderPreset {
        label: "Anthropic",
        endpoint: "https://api.anthropic.com/v1/messages",
        provider_type: "anthropic",
        api_key_env_default: Some("ANTHROPIC_API_KEY"),
    },
    ProviderPreset {
        label: "NVIDIA NIM",
        endpoint: "https://integrate.api.nvidia.com/v1/chat/completions",
        provider_type: "nvidia_nim",
        api_key_env_default: Some("NVIDIA_API_KEY"),
    },
    ProviderPreset {
        label: "Ollama (local)",
        endpoint: "http://localhost:11434/v1/chat/completions",
        provider_type: "ollama",
        api_key_env_default: None,
    },
];

#[derive(serde::Serialize)]
struct QuickstartRoute<'a> {
    model: &'a str,
    endpoint: &'a str,
    provider_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_env: Option<&'a str>,
}

#[derive(serde::Serialize)]
struct QuickstartOutput<'a> {
    routing: BTreeMap<&'a str, QuickstartRoute<'a>>,
}

/// Build the routing config TOML the wizard writes. Pure function so it can
/// be exercised by unit tests without driving stdin. `models` is the
/// `(category, model)` list in the order they should appear; BTreeMap
/// iteration inside the serializer gives a deterministic TOML layout.
pub(crate) fn build_quickstart_toml(
    endpoint: &str,
    provider_type: &str,
    api_key_env: Option<&str>,
    models: &[(String, String)],
) -> String {
    let routing = models
        .iter()
        .map(|(cat, m)| {
            (
                cat.as_str(),
                QuickstartRoute {
                    model: m.as_str(),
                    endpoint,
                    provider_type,
                    api_key_env,
                },
            )
        })
        .collect();
    let output = QuickstartOutput { routing };
    toml::to_string_pretty(&output).expect("QuickstartOutput is always serializable")
}

/// Interactive entry point. Prompts on stdin, writes to a file the user
/// chooses. Returns an error (printed by the caller to stderr) on EOF or
/// user cancellation.
pub fn run_quickstart() -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();

    println!("Welcome to cerebrum quickstart!\n");
    println!("Select your provider:");
    for (i, preset) in PROVIDER_PRESETS.iter().enumerate() {
        println!("  {}. {} ({})", i + 1, preset.label, preset.endpoint);
    }
    println!("  5. Custom\n");

    let choice_raw = prompt(&mut input, "Choice [1-5]", Some("1"))?;
    let choice: usize = choice_raw
        .trim()
        .parse()
        .map_err(|_| format!("invalid choice: {choice_raw:?}"))?;
    if !(1..=5).contains(&choice) {
        return Err(format!("choice must be between 1 and 5, got {choice}"));
    }

    let (endpoint, provider_type, api_key_env_default) = if choice <= PROVIDER_PRESETS.len() {
        let preset = &PROVIDER_PRESETS[choice - 1];
        (
            preset.endpoint.to_string(),
            preset.provider_type.to_string(),
            preset.api_key_env_default.map(|s| s.to_string()),
        )
    } else {
        let endpoint_raw = prompt(
            &mut input,
            "Endpoint URL (e.g. https://api.openai.com/v1/chat/completions)",
            None,
        )?;
        let endpoint = endpoint_raw.trim().to_string();
        if endpoint.is_empty() {
            return Err("endpoint URL cannot be empty".to_string());
        }
        let provider_type_raw = prompt(
            &mut input,
            "Provider type (openai_compatible, anthropic, ollama, nvidia_nim, local)",
            Some("openai_compatible"),
        )?;
        let provider_type = provider_type_raw.trim().to_string();
        if provider_type.is_empty() {
            return Err("provider_type cannot be empty".to_string());
        }
        let api_key_env_raw = prompt(
            &mut input,
            "API key env var name (leave empty if not needed)",
            Some(""),
        )?;
        let api_key_env_trim = api_key_env_raw.trim();
        let api_key_env_default = if api_key_env_trim.is_empty() {
            None
        } else {
            Some(api_key_env_trim.to_string())
        };
        (endpoint, provider_type, api_key_env_default)
    };

    let same_model_raw = prompt(
        &mut input,
        "Use the same model for all 5 routing categories? [Y/n]",
        Some("Y"),
    )?;
    let same_model = !matches!(
        same_model_raw.trim().to_lowercase().as_str(),
        "n" | "no"
    );

    let models: Vec<(String, String)> = if same_model {
        let model_raw = prompt(
            &mut input,
            "Model name (e.g. anthropic/claude-3.5-sonnet)",
            None,
        )?;
        let model = model_raw.trim().to_string();
        if model.is_empty() {
            return Err("model name cannot be empty".to_string());
        }
        ROUTING_CATEGORIES
            .iter()
            .map(|c| (c.to_string(), model.clone()))
            .collect()
    } else {
        let complex_raw = prompt(
            &mut input,
            "Model for COMPLEX_REASONING (your smartest model)",
            None,
        )?;
        let complex = complex_raw.trim().to_string();
        if complex.is_empty() {
            return Err("model name cannot be empty".to_string());
        }
        let rest_raw = prompt(
            &mut input,
            "Model for the other 4 categories (a fast/cheap model)",
            None,
        )?;
        let rest = rest_raw.trim().to_string();
        if rest.is_empty() {
            return Err("model name cannot be empty".to_string());
        }
        ROUTING_CATEGORIES
            .iter()
            .map(|c| {
                let m = if *c == "COMPLEX_REASONING" {
                    complex.clone()
                } else {
                    rest.clone()
                };
                (c.to_string(), m)
            })
            .collect()
    };

    let api_key_env: Option<String> = match api_key_env_default {
        Some(default) => {
            let env_raw = prompt(
                &mut input,
                "Env var holding your API key",
                Some(&default),
            )?;
            let env_trimmed = env_raw.trim();
            if env_trimmed.is_empty() {
                None
            } else {
                Some(env_trimmed.to_string())
            }
        }
        None => None,
    };

    let output_path_raw = prompt(
        &mut input,
        "Output path",
        Some("./cerebrum-config.toml"),
    )?;
    let output_path_str = output_path_raw.trim();
    let output_path_str = if output_path_str.is_empty() {
        "./cerebrum-config.toml"
    } else {
        output_path_str
    };
    let output_path = PathBuf::from(output_path_str);

    let toml = build_quickstart_toml(
        &endpoint,
        &provider_type,
        api_key_env.as_deref(),
        &models,
    );

    println!("\nGenerated config:\n{toml}");

    if output_path.exists() {
        let confirm_raw = prompt(
            &mut input,
            &format!(
                "{} already exists. Overwrite? [y/N]",
                output_path.display()
            ),
            Some("N"),
        )?;
        if !matches!(confirm_raw.trim().to_lowercase().as_str(), "y" | "yes") {
            return Err("aborted: file exists and overwrite not confirmed".to_string());
        }
    }

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create parent directory: {e}"))?;
        }
    }
    std::fs::write(&output_path, &toml)
        .map_err(|e| format!("failed to write {}: {e}", output_path.display()))?;

    eprintln!("Wrote starter config to {}", output_path.display());
    eprintln!("Start with: CONFIG_PATH={} cerebrum", output_path.display());
    Ok(())
}

fn prompt<R: BufRead>(input: &mut R, question: &str, default: Option<&str>) -> Result<String, String> {
    let default_hint = match default {
        Some(d) if !d.is_empty() => format!(" [{d}]"),
        _ => String::new(),
    };
    print!("{question}{default_hint}: ");
    io::stdout()
        .flush()
        .map_err(|e| format!("failed to flush stdout: {e}"))?;
    let mut line = String::new();
    let read = input
        .read_line(&mut line)
        .map_err(|e| format!("failed to read from stdin: {e}"))?;
    if read == 0 {
        return Err("unexpected end of input (Ctrl-D?)".to_string());
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_models() -> Vec<(String, String)> {
        ROUTING_CATEGORIES
            .iter()
            .map(|c| (c.to_string(), "test/model".to_string()))
            .collect()
    }

    #[test]
    fn build_quickstart_toml_with_api_key() {
        let toml = build_quickstart_toml(
            "https://example.com/v1/chat/completions",
            "openai_compatible",
            Some("MY_KEY"),
            &sample_models(),
        );
        let parsed: toml::Value = toml::from_str(&toml).expect("output should be valid TOML");
        let routing = parsed
            .get("routing")
            .and_then(|v| v.as_table())
            .expect("routing table should exist");
        assert_eq!(routing.len(), 5);
        let default = routing
            .get("DEFAULT")
            .and_then(|v| v.as_table())
            .expect("DEFAULT should exist");
        assert_eq!(default.get("model").and_then(|v| v.as_str()), Some("test/model"));
        assert_eq!(
            default.get("endpoint").and_then(|v| v.as_str()),
            Some("https://example.com/v1/chat/completions")
        );
        assert_eq!(
            default.get("provider_type").and_then(|v| v.as_str()),
            Some("openai_compatible")
        );
        assert_eq!(
            default.get("api_key_env").and_then(|v| v.as_str()),
            Some("MY_KEY")
        );
    }

    #[test]
    fn build_quickstart_toml_omits_api_key_when_none() {
        let toml = build_quickstart_toml(
            "http://localhost:11434/v1/chat/completions",
            "ollama",
            None,
            &sample_models(),
        );
        let parsed: toml::Value = toml::from_str(&toml).expect("output should be valid TOML");
        let default = parsed
            .get("routing")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("DEFAULT"))
            .and_then(|v| v.as_table())
            .expect("DEFAULT table should exist");
        assert!(
            default.get("api_key_env").is_none(),
            "api_key_env must be omitted when None, got: {default:?}"
        );
    }

    #[test]
    fn build_quickstart_toml_contains_all_five_categories() {
        let toml = build_quickstart_toml(
            "https://example.com/v1/chat/completions",
            "openai_compatible",
            Some("KEY"),
            &sample_models(),
        );
        for cat in ROUTING_CATEGORIES {
            assert!(
                toml.contains(&format!("[routing.{cat}]")),
                "expected [{cat}] section in generated TOML:\n{toml}"
            );
        }
    }

    #[test]
    fn build_quickstart_toml_different_models_per_category() {
        // Per-category mode: COMPLEX_REASONING uses one model, the rest use another.
        let models = vec![
            ("FILE_READING".to_string(), "fast-model".to_string()),
            ("SYNTAX_FIX".to_string(), "fast-model".to_string()),
            ("COMPLEX_REASONING".to_string(), "smart-model".to_string()),
            ("CASUAL".to_string(), "fast-model".to_string()),
            ("DEFAULT".to_string(), "fast-model".to_string()),
        ];
        let toml = build_quickstart_toml(
            "https://example.com",
            "openai_compatible",
            Some("KEY"),
            &models,
        );
        let parsed: toml::Value = toml::from_str(&toml).expect("valid TOML");
        let routing = parsed.get("routing").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            routing
                .get("COMPLEX_REASONING")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("model"))
                .and_then(|v| v.as_str()),
            Some("smart-model")
        );
        assert_eq!(
            routing
                .get("FILE_READING")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("model"))
                .and_then(|v| v.as_str()),
            Some("fast-model")
        );
    }

    /// Verifies that an end-to-end stdin/stdout session produces the expected
    /// TOML. We run `run_quickstart` with a piped stdin (one provider
    /// choice, model, default path, no overwrite) and capture the file the
    /// wizard would write — but since `run_quickstart` reads from
    /// `io::stdin()` directly, we instead exercise the building blocks
    /// (build_quickstart_toml + file write) to validate the contract.
    #[test]
    fn generated_toml_writes_to_file_and_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "cerebrum-quickstart-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.toml");

        let toml = build_quickstart_toml(
            "https://example.com/v1/chat/completions",
            "openai_compatible",
            Some("MY_KEY"),
            &sample_models(),
        );
        std::fs::write(&path, &toml).unwrap();

        // Re-parse the file we just wrote as a routing config to confirm the
        // end-to-end artifact is usable as a CONFIG_PATH overlay.
        let written = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&written).expect("file should be valid TOML");
        assert!(parsed.get("routing").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
