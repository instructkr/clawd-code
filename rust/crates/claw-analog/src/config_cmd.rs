//! `claw-analog config validate` — parse TOML and profile without calling the API.

use std::path::PathBuf;

use clap::Parser;
use claw_analog::{
    load_analog_toml, load_profile_hint, resolve_analog_options, resolve_analog_profile_path,
    AnalogDoctorOverrides, AnalogFileConfig, AnalogLanguage, OutputFormat,
};

#[derive(Parser, Debug)]
pub struct ValidateCli {
    #[arg(short = 'w', long, default_value = ".", value_name = "DIR")]
    pub workspace: PathBuf,
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Require `<workspace>/.claw-analog.toml` (or `--config`) to exist and parse.
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub strict: bool,
    #[arg(long, value_name = "PATH")]
    pub profile: Option<PathBuf>,
}

pub fn run_validate(cli: ValidateCli) -> i32 {
    let cfg_path = cli
        .config
        .clone()
        .unwrap_or_else(|| cli.workspace.join(".claw-analog.toml"));

    let file_cfg = if cfg_path.is_file() {
        match load_analog_toml(&cfg_path) {
            Ok(c) => {
                println!("OK: {} parses", cfg_path.display());
                c
            }
            Err(e) => {
                eprintln!("ERROR: {}: {e}", cfg_path.display());
                return 1;
            }
        }
    } else if cli.strict {
        eprintln!(
            "ERROR: --strict: config file missing: {}",
            cfg_path.display()
        );
        return 1;
    } else {
        println!(
            "Note: {} absent — using empty TOML defaults for preview",
            cfg_path.display()
        );
        AnalogFileConfig::default()
    };

    let prof_path = resolve_analog_profile_path(
        &cli.workspace,
        cli.profile.clone(),
        file_cfg.profile.as_deref(),
    );
    let mut ok = true;
    match &prof_path {
        None => println!(
            "Profile: (none — no CLI/TOML path and no default ~/.claw-analog/profile.toml)"
        ),
        Some(p) => match load_profile_hint(p) {
            Ok(Some(line)) => println!(
                "OK: profile {} (line: {} chars)",
                p.display(),
                line.chars().count()
            ),
            Ok(None) => println!("OK: profile {} (empty `line`)", p.display()),
            Err(e) => {
                eprintln!("ERROR: profile {}: {e}", p.display());
                ok = false;
            }
        },
    }

    let lang = file_cfg
        .language
        .as_deref()
        .and_then(AnalogLanguage::from_toml_str)
        .unwrap_or_default();

    let r = resolve_analog_options(&file_cfg, &AnalogDoctorOverrides::default());
    println!("\nMerge preview (TOML + defaults only; main-run CLI flags not applied):");
    println!("  language (TOML): {}", lang.as_str());
    println!("  model: {}", r.model);
    println!("  permission: {}", r.permission_mode.as_str());
    println!("  preset: {}", r.preset.label().unwrap_or("none"));
    println!(
        "  output_format: {}",
        match r.output_format {
            OutputFormat::Rich => "rich",
            OutputFormat::Json => "json",
        }
    );
    println!("  stream: {}", r.use_stream);
    println!(
        "  runtime_enforcer: {}",
        if r.use_runtime_enforcer { "on" } else { "off" }
    );
    println!(
        "  accept_danger_non_interactive: {}",
        r.accept_danger_non_interactive
    );
    println!("  Provenance:");
    for line in &r.provenance {
        println!("    - {line}");
    }

    i32::from(!ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_fails_when_config_missing() {
        let dir = tempfile::tempdir().unwrap();
        let code = run_validate(ValidateCli {
            workspace: dir.path().to_path_buf(),
            config: None,
            strict: true,
            profile: None,
        });
        assert_eq!(code, 1);
    }

    #[test]
    fn parses_when_config_present() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".claw-analog.toml");
        std::fs::write(&p, r#"model = "sonnet""#).unwrap();
        let code = run_validate(ValidateCli {
            workspace: dir.path().to_path_buf(),
            config: None,
            strict: true,
            profile: None,
        });
        assert_eq!(code, 0);
    }
}
