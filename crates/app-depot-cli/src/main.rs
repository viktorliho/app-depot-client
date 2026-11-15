use app_depot_core::{
    api::{ApiClient, ApiError},
    models::{App, AssociationArtifact, Capabilities, DeclarationSnapshot, Release},
};
use serde::Serialize;
use std::{env, process::ExitCode};

struct Options {
    server: String,
    allow_insecure_http: bool,
    json: bool,
    command: String,
    id: Option<String>,
}

fn usage() -> &'static str {
    "usage: app-depot --server URL [--json] [--allow-insecure-http] <command> [id]\n\
     commands: capabilities | apps | app ID | releases APP_ID | release ID | artifacts RELEASE_ID | declarations RELEASE_ID\n\
     optional Bearer token: APP_DEPOT_TOKEN environment variable"
}

fn parse_args() -> Result<Options, ApiError> {
    let mut args = env::args().skip(1).peekable();
    let mut server = env::var("APP_DEPOT_SERVER_URL").ok();
    let mut allow_insecure_http = false;
    let mut json = false;
    let mut positional = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => {
                server = Some(
                    args.next()
                        .ok_or_else(|| ApiError::Config(usage().into()))?,
                );
            }
            "--json" => json = true,
            "--allow-insecure-http" => allow_insecure_http = true,
            "-h" | "--help" => return Err(ApiError::Config(usage().into())),
            _ if arg.starts_with('-') => {
                return Err(ApiError::Config(format!(
                    "unknown option {arg}\n{}",
                    usage()
                )));
            }
            _ => positional.push(arg),
        }
    }
    let command = positional
        .first()
        .ok_or_else(|| ApiError::Config(usage().into()))?
        .clone();
    let needed = match command.as_str() {
        "capabilities" | "apps" => 1,
        "app" | "releases" | "release" | "artifacts" | "declarations" => 2,
        _ => return Err(ApiError::Config(usage().into())),
    };
    if positional.len() != needed {
        return Err(ApiError::Config(usage().into()));
    }
    Ok(Options {
        server: server
            .ok_or_else(|| ApiError::Config(format!("server URL required\n{}", usage())))?,
        allow_insecure_http,
        json,
        command,
        id: positional.get(1).cloned(),
    })
}

fn print_json<T: Serialize>(value: &T) -> Result<(), ApiError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| ApiError::Protocol(error.to_string()))?
    );
    Ok(())
}

fn print_capabilities(caps: &Capabilities) {
    println!("API version: {}", caps.api_version);
    println!("File identity: {}", caps.file_identity);
    println!("Tree identity: {}", caps.tree_identity);
    println!("Download ranges: {}", caps.download_ranges);
    println!(
        "Limits: max page {}, max concurrent ranges {}, max upload bytes {}",
        caps.limits.max_page_size, caps.limits.max_concurrent_ranges, caps.limits.max_upload_bytes
    );
    println!(
        "Features: {}",
        if caps.features.is_empty() {
            "(none)".into()
        } else {
            caps.features.join(", ")
        }
    );
    for issue in caps.incompatibilities() {
        println!("Incompatible: {issue}");
    }
}

fn print_apps(apps: &[App]) {
    for app in apps {
        println!(
            "{}  {}  [{} / {}]",
            app.id, app.name, app.visibility, app.state
        );
    }
}

fn print_releases(releases: &[Release]) {
    for release in releases {
        println!(
            "{}  {}  [{}]",
            release.id,
            release.version.as_deref().unwrap_or("(unversioned)"),
            release.state
        );
    }
}

fn print_artifacts(items: &[AssociationArtifact]) {
    for item in items {
        println!(
            "association {}  artifact {}  {}  size {}",
            item.association.id,
            item.artifact.id,
            item.artifact.kind,
            item.artifact
                .size
                .map_or_else(|| "unknown".into(), |size| size.to_string())
        );
    }
}

fn print_declarations(snapshot: &DeclarationSnapshot) -> Result<(), ApiError> {
    println!("Release {}", snapshot.release_id);
    for (scope, data) in [
        ("App", &snapshot.app.data),
        ("Release", &snapshot.release.data),
    ] {
        println!(
            "{scope}: {}",
            serde_json::to_string_pretty(data)
                .map_err(|error| ApiError::Protocol(error.to_string()))?
        );
    }
    for association in &snapshot.associations {
        println!(
            "Association {}: {}",
            association.association_id,
            serde_json::to_string_pretty(&association.declaration.data)
                .map_err(|error| ApiError::Protocol(error.to_string()))?
        );
    }
    Ok(())
}

fn run() -> Result<(), ApiError> {
    let options = parse_args()?;
    let token = env::var("APP_DEPOT_TOKEN")
        .ok()
        .filter(|token| !token.is_empty());
    let client = ApiClient::new(&options.server, token, options.allow_insecure_http)?;
    if options.command == "capabilities" {
        let caps = client.capabilities()?;
        if options.json {
            print_json(&caps)?;
        } else {
            print_capabilities(&caps);
        }
        let issues = caps.incompatibilities();
        if !issues.is_empty() {
            return Err(ApiError::Incompatible(issues.join("; ")));
        }
        return Ok(());
    }
    let caps = client.ensure_read_compatible()?;
    let page_size = caps.page_size().map_err(ApiError::Incompatible)?;
    let id = options.id.as_deref().unwrap_or("");
    match options.command.as_str() {
        "apps" => {
            let apps = client.list_apps(page_size)?;
            if options.json {
                print_json(&apps)
            } else {
                print_apps(&apps);
                Ok(())
            }
        }
        "app" => {
            let app = client.app(id)?;
            if options.json {
                print_json(&app)
            } else {
                print_apps(&[app]);
                Ok(())
            }
        }
        "releases" => {
            let releases = client.list_releases(id, page_size)?;
            if options.json {
                print_json(&releases)
            } else {
                print_releases(&releases);
                Ok(())
            }
        }
        "release" => {
            let release = client.release(id)?;
            if options.json {
                print_json(&release)
            } else {
                print_releases(&[release]);
                Ok(())
            }
        }
        "artifacts" => {
            let artifacts = client.associated_artifacts(id, page_size)?;
            if options.json {
                print_json(&artifacts)
            } else {
                print_artifacts(&artifacts);
                Ok(())
            }
        }
        "declarations" => {
            let snapshot = client.declarations(id)?;
            if options.json {
                print_json(&snapshot)
            } else {
                print_declarations(&snapshot)
            }
        }
        _ => Err(ApiError::Config(usage().into())),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(match error {
                ApiError::Config(_) => 2,
                ApiError::Authentication(_) => 3,
                ApiError::Authorization(_) => 4,
                ApiError::Transport(_) => 5,
                ApiError::Incompatible(_) => 6,
                ApiError::Protocol(_) => 7,
                ApiError::Http { .. } => 8,
            })
        }
    }
}
