#[cfg(all(feature = "decky", target_os = "linux"))]
mod decky;

use app_depot_core::{
    ServerProfileId,
    api::{ApiClient, ApiError},
    launch::LaunchCandidate,
    library::{Library, LibraryError, LibraryPaths},
    models::{App, AssociationArtifact, Capabilities, DeclarationSnapshot, Release},
    secrets::{NativeSecrets, SecretError, SecretStore},
    transfer::DownloadJob,
};
use serde::Serialize;
use std::{
    env,
    io::{self, IsTerminal, Read},
    path::PathBuf,
    process::ExitCode,
};

struct Options {
    server: Option<String>,
    profile: Option<ServerProfileId>,
    library_data: Option<PathBuf>,
    library_content: Option<PathBuf>,
    allow_insecure_http: bool,
    json: bool,
    confirm: bool,
    all: bool,
    token_stdin: bool,
    pause_after: Option<u64>,
    positional: Vec<String>,
}

fn usage() -> &'static str {
    "usage: app-depot [--server URL | --profile ID] [--json] [--allow-insecure-http] <command> [id]\n\
     read: capabilities | apps | app ID | releases APP_ID | release ID | artifacts RELEASE_ID | declarations RELEASE_ID\n\
     local: profile add NAME URL | profile list [--all] | profile rename ID NAME | profile move ID URL | profile remove ID --confirm\n\
     credentials: profile token-set ID --token-stdin | profile token-remove ID\n\
     sync/offline: --profile ID sync | cached-apps | cached-releases APP_ID | cached-declarations RELEASE_ID\n\
     download: --profile ID download start RELEASE_ID ASSOCIATION_ID [--pause-after-bytes N] | enqueue RELEASE_ID ASSOCIATION_ID | jobs | files | trees | tree-files JOB_ID | move-top JOB_ID | pause JOB_ID | resume JOB_ID | retry JOB_ID | cancel JOB_ID\n\
     launch: --profile ID launch list RELEASE_ID | launch play RELEASE_ID ASSOCIATION_ID ENTRY_POINT_ID\n\
     prototype: --profile ID first-play APP_ID RELEASE_ID ASSOCIATION_ID ENTRY_POINT_ID\n\
     runtime: --profile ID runtime list | runtime prepare ASSOCIATION_ID | runtime inspect INSTALLATION_ID | runtime verify INSTALLATION_ID | runtime preview INSTALLATION_ID | runtime preview-complete INSTALLATION_ID | runtime remove INSTALLATION_ID --confirm | runtime remove-complete INSTALLATION_ID --confirm | runtime remove-artifact ASSOCIATION_ID --confirm | runtime remove-app APP_ID --confirm | runtime remove-app-complete APP_ID --confirm | runtime repair INSTALLATION_ID --confirm | runtime repair-source ASSOCIATION_ID --confirm\n\
     library: library info | library usage | library gc --confirm | library migration-preview DESTINATION_ROOT | library migrate DESTINATION_ROOT --confirm (source selected by --library-data/--library-content)\n\
     optional APP_DEPOT_TOKEN overrides stored token for this process; --library-data/--library-content select local roots"
}

fn parse_args() -> Result<Options, ApiError> {
    let mut args = env::args().skip(1).peekable();
    let mut server = env::var("APP_DEPOT_SERVER_URL").ok();
    let mut explicit_server = false;
    let mut profile = None;
    let mut library_data = env::var_os("APP_DEPOT_LIBRARY_DATA_DIR").map(PathBuf::from);
    let mut library_content = env::var_os("APP_DEPOT_LIBRARY_CONTENT_DIR").map(PathBuf::from);
    let mut allow_insecure_http = false;
    let mut json = false;
    let mut confirm = false;
    let mut all = false;
    let mut token_stdin = false;
    let mut pause_after = None;
    let mut positional = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => {
                explicit_server = true;
                server = Some(
                    args.next()
                        .ok_or_else(|| ApiError::Config(usage().into()))?,
                );
            }
            "--profile" => {
                profile = Some(ServerProfileId(
                    args.next()
                        .ok_or_else(|| ApiError::Config(usage().into()))?,
                ))
            }
            "--library-data" => {
                library_data = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| ApiError::Config(usage().into()))?,
                ))
            }
            "--library-content" => {
                library_content = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| ApiError::Config(usage().into()))?,
                ))
            }
            "--json" => json = true,
            "--allow-insecure-http" => allow_insecure_http = true,
            "--confirm" => confirm = true,
            "--all" => all = true,
            "--token-stdin" => token_stdin = true,
            "--pause-after-bytes" => {
                pause_after = Some(
                    args.next()
                        .ok_or_else(|| ApiError::Config(usage().into()))?
                        .parse()
                        .map_err(|_| ApiError::Config("invalid pause byte count".into()))?,
                )
            }
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
    if positional.is_empty() {
        return Err(ApiError::Config(usage().into()));
    }
    if profile.is_some() && explicit_server {
        return Err(ApiError::Config(
            "--server and --profile are mutually exclusive".into(),
        ));
    }
    if profile.is_some() && !explicit_server {
        server = None;
    }
    Ok(Options {
        server: server.take(),
        profile,
        library_data,
        library_content,
        allow_insecure_http,
        json,
        confirm,
        all,
        token_stdin,
        pause_after,
        positional,
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

enum CliError {
    Api(ApiError),
    Library(LibraryError),
    Secret(SecretError),
    #[cfg(all(feature = "decky", target_os = "linux"))]
    Child(std::process::ExitStatus),
}
fn api_exit(error: &ApiError) -> u8 {
    match error {
        ApiError::Config(_) => 2,
        ApiError::Authentication(_) => 3,
        ApiError::Authorization(_) => 4,
        ApiError::Transport(_) => 5,
        ApiError::Incompatible(_) => 6,
        ApiError::Protocol(_) => 7,
        ApiError::Http { .. } => 8,
    }
}
fn api_connection_status(error: &ApiError) -> &'static str {
    match error {
        ApiError::Transport(_) => "unreachable",
        ApiError::Authentication(_) => "authentication-required",
        ApiError::Authorization(_) => "forbidden",
        ApiError::Incompatible(_) => "incompatible",
        ApiError::Protocol(_) => "protocol-error",
        _ => "http-error",
    }
}
impl From<ApiError> for CliError {
    fn from(value: ApiError) -> Self {
        Self::Api(value)
    }
}
impl From<LibraryError> for CliError {
    fn from(value: LibraryError) -> Self {
        Self::Library(value)
    }
}
impl From<SecretError> for CliError {
    fn from(value: SecretError) -> Self {
        Self::Secret(value)
    }
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(error) => error.fmt(f),
            Self::Library(error) => error.fmt(f),
            Self::Secret(error) => error.fmt(f),
            #[cfg(all(feature = "decky", target_os = "linux"))]
            Self::Child(status) => write!(f, "Application exited with {status}"),
        }
    }
}

fn selected_library_paths(options: &Options) -> Result<LibraryPaths, CliError> {
    if let Some(path) = &options.library_data {
        return Ok(LibraryPaths {
            metadata: path.clone(),
            content: options
                .library_content
                .clone()
                .unwrap_or_else(|| path.clone()),
        });
    }
    let mut paths = LibraryPaths::default_for_user()?;
    if let Some(path) = &options.library_content {
        paths.content = path.clone();
    }
    Ok(paths)
}

fn open_library(options: &Options) -> Result<Library, CliError> {
    Ok(Library::open(selected_library_paths(options)?)?)
}

fn selected_token(profile: &app_depot_core::library::Profile) -> Result<Option<String>, CliError> {
    if let Some(token) = env::var("APP_DEPOT_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(token));
    }
    match profile.credential_ref.as_deref() {
        None => Ok(None),
        Some(reference) => match NativeSecrets.get(reference) {
            Ok(token) => Ok(Some(token)),
            Err(SecretError::Unavailable) => {
                eprintln!("native secure storage unavailable; trying anonymous access");
                Ok(None)
            }
            Err(error) => Err(error.into()),
        },
    }
}

fn run_profile(options: &Options) -> Result<(), CliError> {
    let mut library = open_library(options)?;
    let args = &options.positional;
    let action = args.get(1).map(String::as_str).unwrap_or("");
    match (action, args.len()) {
        ("add", 4) => {
            let profile = library.add_profile(&args[2], &args[3], options.allow_insecure_http)?;
            if options.json {
                print_json(&profile)?;
            } else {
                println!("{}  {}  {}", profile.id.0, profile.name, profile.origin);
            }
        }
        ("list", 2) => {
            let profiles = library.profiles(options.all)?;
            if options.json {
                print_json(&profiles)?;
            } else {
                for profile in profiles {
                    println!(
                        "{}  {}  {}  {}{}",
                        profile.id.0,
                        profile.name,
                        profile.origin,
                        profile.connection_status,
                        if profile.archived { " (archived)" } else { "" }
                    );
                }
            }
        }
        ("rename", 4) => library.rename_profile(&ServerProfileId(args[2].clone()), &args[3])?,
        ("move", 4) => {
            let id = ServerProfileId(args[2].clone());
            if library.profile(&id)?.credential_ref.is_some() {
                return Err(CliError::Library(LibraryError::Invalid(
                    "remove the old profile token before changing its URL".into(),
                )));
            }
            let profile = library.replace_origin(&id, &args[3], options.allow_insecure_http)?;
            if options.json {
                print_json(&profile)?;
            } else {
                println!("new profile ID: {} (old profile archived)", profile.id.0);
            }
        }
        ("remove", 3) => {
            if !options.confirm {
                return Err(ApiError::Config(
                    "profile removal requires --confirm; cached data stays archived".into(),
                )
                .into());
            }
            let id = ServerProfileId(args[2].clone());
            let profile = library.profile(&id)?;
            if profile.archived {
                return Err(LibraryError::Invalid("profile is already archived".into()).into());
            }
            if let Some(reference) = profile.credential_ref {
                NativeSecrets.delete(&reference)?;
                library.set_credential_ref(&id, None)?;
            }
            library.archive_profile(&id)?;
            println!("profile archived; cached metadata retained for offline reading");
        }
        ("token-set", 3) => {
            if !options.token_stdin || io::stdin().is_terminal() {
                return Err(ApiError::Config(
                    "token-set requires --token-stdin with piped input".into(),
                )
                .into());
            }
            let id = ServerProfileId(args[2].clone());
            let old = library.profile(&id)?.credential_ref;
            let mut token = String::new();
            io::stdin()
                .take(16_385)
                .read_to_string(&mut token)
                .map_err(LibraryError::Io)?;
            if token.len() > 16_384 {
                return Err(SecretError::Invalid.into());
            }
            let token = token.trim_end_matches(['\r', '\n']);
            let mut secrets = NativeSecrets;
            if let Some(old) = old {
                secrets.replace(&old, token)?;
            } else {
                let reference = secrets.put(token)?;
                if let Err(error) = library.set_credential_ref(&id, Some(&reference)) {
                    let _ = secrets.delete(&reference);
                    return Err(error.into());
                }
            }
            println!("token stored in native secure storage for profile {}", id.0);
        }
        ("token-remove", 3) => {
            let id = ServerProfileId(args[2].clone());
            if let Some(reference) = library.profile(&id)?.credential_ref {
                NativeSecrets.delete(&reference)?;
                library.set_credential_ref(&id, None)?;
            }
            println!("profile token removed");
        }
        _ => return Err(ApiError::Config(usage().into()).into()),
    }
    Ok(())
}

fn run_cached(options: &Options, profile_id: &ServerProfileId) -> Result<(), CliError> {
    let library = open_library(options)?;
    let args = &options.positional;
    match (args[0].as_str(), args.len()) {
        ("cached-apps", 1) => {
            let rows = library.cached_apps(profile_id)?;
            if options.json {
                print_json(&rows)?;
            } else {
                for row in rows {
                    println!(
                        "{}  {}  [cached; last sync {:?}; stale {}]",
                        row.value.id, row.value.name, row.last_sync, row.stale
                    );
                }
            }
        }
        ("cached-releases", 2) => {
            let rows = library.cached_releases(profile_id, &args[1])?;
            if options.json {
                print_json(&rows)?;
            } else {
                for row in rows {
                    println!(
                        "{}  {}  [cached; last sync {:?}; stale {}]",
                        row.value.id,
                        row.value.version.as_deref().unwrap_or("(unversioned)"),
                        row.last_sync,
                        row.stale
                    );
                }
            }
        }
        ("cached-declarations", 2) => {
            let row = library
                .cached_snapshot(profile_id, &args[1])?
                .ok_or_else(|| LibraryError::Invalid("cached declaration not found".into()))?;
            if options.json {
                print_json(&row)?;
            } else {
                println!("cached; last sync {:?}; stale {}", row.last_sync, row.stale);
                print_declarations(&row.value)?;
            }
        }
        _ => return Err(ApiError::Config(usage().into()).into()),
    }
    Ok(())
}

fn print_job(job: &DownloadJob, json: bool) -> Result<(), CliError> {
    if json {
        print_json(job)?;
    } else {
        println!(
            "{}  {}  {}/{} bytes  {}",
            job.id, job.state, job.progress, job.expected_size, job.artifact_id
        );
    }
    Ok(())
}

fn run_download(options: &Options) -> Result<(), CliError> {
    let profile_id = options
        .profile
        .as_ref()
        .ok_or_else(|| ApiError::Config("download requires --profile ID".into()))?;
    let args = &options.positional;
    let mut library = open_library(options)?;
    library.reconcile_file_jobs()?;
    library.reconcile_tree_jobs()?;
    match (args.get(1).map(String::as_str), args.len()) {
        (Some("jobs"), 2) => {
            let jobs = library.jobs(Some(profile_id))?;
            if options.json {
                print_json(&jobs)?;
            } else {
                for job in jobs {
                    print_job(&job, false)?;
                }
            }
            Ok(())
        }
        (Some("files"), 2) => {
            let files = library.verified_files(Some(profile_id))?;
            if options.json {
                print_json(&files)?;
            } else {
                for file in files {
                    println!(
                        "{}  {} bytes  {}",
                        file.artifact_id,
                        file.size,
                        file.path.display()
                    );
                }
            }
            Ok(())
        }
        (Some("trees"), 2) => {
            let trees = library.verified_trees(Some(profile_id))?;
            if options.json {
                print_json(&trees)?;
            } else {
                for tree in trees {
                    println!(
                        "{}  {} files  {}",
                        tree.artifact_id,
                        tree.file_count,
                        tree.path.display()
                    );
                }
            }
            Ok(())
        }
        (Some("tree-files"), 3) => {
            let job = library.job(&args[2])?;
            if &job.profile_id != profile_id {
                return Err(LibraryError::Invalid("job belongs to another profile".into()).into());
            }
            let files = library.tree_files(&args[2])?;
            if options.json {
                print_json(&files)?;
            } else {
                for file in files {
                    println!(
                        "{}  {}/{} bytes  {}",
                        if file.verified { "Verified" } else { "Pending" },
                        file.progress,
                        file.size,
                        file.path
                    );
                }
            }
            Ok(())
        }
        (Some("pause"), 3) => {
            let job = library.job(&args[2])?;
            if &job.profile_id != profile_id {
                return Err(LibraryError::Invalid("job belongs to another profile".into()).into());
            }
            print_job(&library.pause_job(&args[2])?, options.json)
        }
        (Some("move-top"), 3) => {
            let job = library.job(&args[2])?;
            if &job.profile_id != profile_id {
                return Err(LibraryError::Invalid("job belongs to another profile".into()).into());
            }
            print_job(&library.move_job_to_top(&args[2])?, options.json)
        }
        (Some("cancel"), 3) => {
            let job = library.job(&args[2])?;
            if &job.profile_id != profile_id {
                return Err(LibraryError::Invalid("job belongs to another profile".into()).into());
            }
            print_job(&library.cancel_job(&args[2])?, options.json)
        }
        (Some("start" | "enqueue"), 4) | (Some("resume" | "retry"), 3) => {
            let profile = library.profile(profile_id)?;
            if profile.archived {
                return Err(
                    LibraryError::Invalid("archived profile is offline-only".into()).into(),
                );
            }
            let client = ApiClient::new(
                &profile.origin,
                selected_token(&profile)?,
                options.allow_insecure_http,
            )?;
            let job = if matches!(args[1].as_str(), "start" | "enqueue") {
                let caps = client.ensure_read_compatible()?;
                let association = client
                    .list_associations(&args[2], caps.page_size().map_err(ApiError::Incompatible)?)?
                    .into_iter()
                    .find(|a| a.id == args[3])
                    .ok_or_else(|| {
                        LibraryError::Invalid(
                            "Association is absent from accessible Release".into(),
                        )
                    })?;
                let artifact = client.artifact(&association.artifact_id)?;
                if artifact.kind == "Tree" {
                    library.queue_tree(&client, profile_id, &args[2], &args[3])?
                } else {
                    library.queue_file(&client, profile_id, &args[2], &args[3])?
                }
            } else {
                let job = library.job(&args[2])?;
                if &job.profile_id != profile_id {
                    return Err(
                        LibraryError::Invalid("job belongs to another profile".into()).into(),
                    );
                }
                library.retry_job(&args[2])?
            };
            if matches!(args[1].as_str(), "start" | "enqueue") && job.state == "Queued" {
                library.move_job_to_top(&job.id)?;
            }
            if args[1] == "enqueue" {
                return print_job(&job, options.json);
            }
            if job.state != "Queued" {
                return print_job(&job, options.json);
            }
            eprintln!("download job: {}", job.id);
            let result = if job.artifact_id.starts_with("adtree:v1:sha256:") {
                library.run_tree_job(&client, &job.id, options.pause_after)
            } else {
                library.run_file_job(&client, &job.id, options.pause_after)
            };
            let final_job = match result {
                Ok(job) => job,
                Err(LibraryError::Invalid(message))
                    if message == "another application download holds the library slot" =>
                {
                    eprintln!(
                        "another transfer is active; this job remains queued for the desktop scheduler"
                    );
                    library.job(&job.id)?
                }
                Err(error) => return Err(error.into()),
            };
            print_job(&final_job, options.json)
        }
        _ => Err(ApiError::Config(usage().into()).into()),
    }
}

fn run_launch(options: &Options) -> Result<(), CliError> {
    let profile = options
        .profile
        .as_ref()
        .ok_or_else(|| ApiError::Config("launch requires --profile ID".into()))?;
    let args = &options.positional;
    if !matches!(
        (args.get(1).map(String::as_str), args.len()),
        (Some("list"), 3) | (Some("play"), 5)
    ) {
        return Err(ApiError::Config(usage().into()).into());
    }
    let mut library = open_library(options)?;
    let candidates = library.launch_candidates(profile, &args[2])?;
    if args[1] == "list" {
        if options.json {
            print_json(&candidates)?
        } else {
            for candidate in candidates {
                println!(
                    "{}  {}  {}  {}  {}",
                    candidate.association_id,
                    candidate.entry_id,
                    candidate.status,
                    candidate.target.as_deref().unwrap_or("(none)"),
                    candidate.reason
                )
            }
        }
        return Ok(());
    }
    let selected = candidates
        .into_iter()
        .find(|c| c.association_id == args[3] && c.entry_id == args[4])
        .ok_or_else(|| {
            LibraryError::Invalid(
                "selected Association and entry-point ID not found in cached Release snapshot"
                    .into(),
            )
        })?;
    play_selected(&mut library, &selected)
}

fn run_first_play(options: &Options, prepare_only: bool) -> Result<(), CliError> {
    let profile_id = options
        .profile
        .as_ref()
        .ok_or_else(|| ApiError::Config("first-play requires --profile ID".into()))?;
    let (app, release, association, entry, revision) = match options.positional.as_slice() {
        [_, app, release, association, entry] if !prepare_only => {
            (app, release, association, entry, None)
        }
        [_, app, release, association, entry, revision] if prepare_only => {
            (app, release, association, entry, Some(revision))
        }
        _ => return Err(ApiError::Config(usage().into()).into()),
    };
    let mut library = open_library(options)?;
    let cached_release = library
        .cached_releases(profile_id, app)?
        .into_iter()
        .find(|row| row.value.id == *release)
        .ok_or_else(|| {
            LibraryError::Invalid("selected Release is absent from cached App".into())
        })?;
    let selected = library
        .launch_candidates(profile_id, release)?
        .into_iter()
        .find(|candidate| candidate.association_id == *association && candidate.entry_id == *entry)
        .ok_or_else(|| {
            LibraryError::Invalid("selected entry point is absent from cached Release".into())
        })?;
    if revision.is_some_and(|revision| *revision != selected.declaration_revision) {
        return Err(LibraryError::Invalid(
            "Launch declaration changed. Synchronize and choose again.".into(),
        )
        .into());
    }
    if selected.status == "NotDownloaded" {
        let profile = library.profile(profile_id)?;
        if profile.archived || cached_release.stale {
            return Err(LibraryError::Invalid(
                "selected Release is offline-only; refresh the catalog before downloading".into(),
            )
            .into());
        }
        let client = ApiClient::new(
            &profile.origin,
            selected_token(&profile)?,
            options.allow_insecure_http,
        )?;
        client.ensure_read_compatible()?;
        let cached = library
            .cached_snapshot(profile_id, release)?
            .ok_or_else(|| LibraryError::Invalid("cached declaration unavailable".into()))?;
        let live = client.declarations(release)?;
        if client.release(release)?.app_id != *app
            || live.app.revision_id != cached.value.app.revision_id
            || live.release.revision_id != cached.value.release.revision_id
            || !live.associations.iter().any(|scope| {
                scope.association_id == *association
                    && scope.declaration.revision_id == selected.declaration_revision
            })
        {
            return Err(LibraryError::Invalid(
                "selected declaration changed; refresh the catalog".into(),
            )
            .into());
        }
        library.reconcile_file_jobs()?;
        library.reconcile_tree_jobs()?;
        let is_tree = selected.artifact_id.starts_with("adtree:v1:sha256:");
        let job = if is_tree {
            library.queue_tree(&client, profile_id, release, association)?
        } else {
            library.queue_file(&client, profile_id, release, association)?
        };
        if job.artifact_id != selected.artifact_id {
            return Err(LibraryError::Invalid(
                "selected Artifact changed; refresh the catalog".into(),
            )
            .into());
        }
        if job.state == "Queued" {
            library.move_job_to_top(&job.id)?;
            eprintln!("downloading Artifact");
            if is_tree {
                library.run_tree_job(&client, &job.id, None)?;
            } else {
                library.run_file_job(&client, &job.id, None)?;
            }
        }
        let refreshed = library
            .launch_candidates(profile_id, release)?
            .into_iter()
            .find(|candidate| {
                candidate.association_id == *association
                    && candidate.entry_id == *entry
                    && candidate.artifact_id == selected.artifact_id
                    && candidate.declaration_revision == selected.declaration_revision
            })
            .ok_or_else(|| {
                LibraryError::Invalid("cached selection changed during download".into())
            })?;
        return finish_first_play(&mut library, &refreshed, prepare_only);
    }
    finish_first_play(&mut library, &selected, prepare_only)
}

fn finish_first_play(
    library: &mut Library,
    selected: &LaunchCandidate,
    prepare_only: bool,
) -> Result<(), CliError> {
    if prepare_only {
        return prepare_selected(library, selected);
    }
    play_selected(library, selected)
}

fn prepare_selected(library: &mut Library, selected: &LaunchCandidate) -> Result<(), CliError> {
    if selected.status == "PreparationRequired" {
        eprintln!("preparing writable installation from verified local content");
        library.prepare_portable(&selected.profile_id, &selected.association_id)?;
    } else if selected.status != "Ready" {
        return Err(LibraryError::Invalid(format!(
            "launch unavailable: {} ({})",
            selected.status, selected.reason
        ))
        .into());
    }
    Ok(())
}

fn play_selected(library: &mut Library, selected: &LaunchCandidate) -> Result<(), CliError> {
    prepare_selected(library, selected)?;
    let mut child = library.play_declared(
        &selected.profile_id,
        &selected.release_id,
        &selected.association_id,
        &selected.entry_id,
        &selected.declaration_revision,
    )?;
    eprintln!("started process {}", child.id());
    let status = child.wait().map_err(LibraryError::Io)?;
    if status.success() {
        println!("process exited successfully");
        Ok(())
    } else {
        Err(LibraryError::Invalid(format!("process exited with {status}")).into())
    }
}

fn run_runtime(options: &Options) -> Result<(), CliError> {
    let profile = options
        .profile
        .as_ref()
        .ok_or_else(|| ApiError::Config("runtime requires --profile ID".into()))?;
    let args = &options.positional;
    let mut library = open_library(options)?;
    match (args.get(1).map(String::as_str), args.len()) {
        (Some("list"), 2) => {
            let items = library.installations(Some(profile))?;
            if options.json {
                print_json(&items)?
            } else {
                for item in items {
                    println!(
                        "{}  {}  {}  {}",
                        item.id,
                        item.app_id,
                        item.state,
                        item.path.display()
                    );
                }
            }
        }
        (Some("inspect"), 3) => {
            let item = library.installation(&args[2])?;
            if &item.profile_id != profile {
                return Err(LibraryError::Invalid(
                    "installation belongs to another profile".into(),
                )
                .into());
            }
            if options.json {
                print_json(&item)?
            } else {
                println!(
                    "{}  {}  {}  {}",
                    item.id,
                    item.app_id,
                    item.state,
                    item.path.display()
                );
            }
        }
        (Some("verify"), 3) => {
            let item = library.installation(&args[2])?;
            if &item.profile_id != profile {
                return Err(LibraryError::Invalid(
                    "installation belongs to another profile".into(),
                )
                .into());
            }
            let result = library.verify_installation(&args[2])?;
            if options.json {
                print_json(&result)?;
            } else {
                println!(
                    "{}",
                    if result.matches_original() {
                        "Matches published Artifact"
                    } else {
                        "Local installation differs from published Artifact"
                    }
                );
                for path in &result.missing {
                    println!("Missing: {path}");
                }
                for path in &result.modified {
                    println!("Modified: {path}");
                }
                for path in &result.unexpected {
                    println!("Unexpected: {path}");
                }
            }
        }
        (Some("preview" | "preview-complete"), 3) => {
            let preview = library.removal_preview(&args[2], args[1] == "preview-complete")?;
            if &preview.installation.profile_id != profile {
                return Err(LibraryError::Invalid(
                    "installation belongs to another profile".into(),
                )
                .into());
            }
            if options.json {
                print_json(&preview)?;
            } else {
                println!(
                    "Selected: {} / {} / {}\nWorking copy: {} ({} logical bytes)",
                    preview.installation.app_id,
                    preview.installation.release_id,
                    preview.installation.association_id,
                    preview.installation.path.display(),
                    preview.working_copy_logical_bytes
                );
                if preview.complete {
                    println!(
                        "Managed user data: {} ({} logical bytes)\nVerified materialization: {} logical bytes\nOther verified references to this Artifact: {}",
                        preview.installation.userdata.display(),
                        preview.managed_data_logical_bytes,
                        preview.verified_materialization_logical_bytes,
                        preview.other_verified_references
                    );
                } else {
                    println!("Managed user data and verified download remain.");
                }
                println!(
                    "This is a preview. Physical disk space reclaimed may differ; the Server is unchanged."
                );
            }
        }
        (Some("prepare"), 3) => {
            let item = library.prepare_portable(profile, &args[2])?;
            if options.json {
                print_json(&item)?
            } else {
                println!("prepared {} at {}", item.id, item.path.display());
            }
        }
        (Some("repair-source"), 3) => {
            if !options.confirm {
                return Err(LibraryError::Invalid(
                    "source repair requires --confirm; the old directory is preserved in staging"
                        .into(),
                )
                .into());
            }
            let (path, quarantine) = library.repair_verified_tree(profile, &args[2])?;
            println!("verified source repaired at {}", path.display());
            if let Some(quarantine) = quarantine {
                println!("previous directory preserved at {}", quarantine.display());
            }
        }
        (Some("remove-artifact"), 3) => {
            if !options.confirm {
                return Err(LibraryError::Invalid(
                    "local Artifact removal requires --confirm".into(),
                )
                .into());
            }
            library.remove_local_artifact(profile, &args[2])?;
            println!("local Association download removed; Server and other local owners unchanged");
        }
        (Some("remove-app" | "remove-app-complete"), 3) => {
            if !options.confirm {
                return Err(
                    LibraryError::Invalid("local App removal requires --confirm".into()).into(),
                );
            }
            let count =
                library.remove_local_app(profile, &args[2], args[1] == "remove-app-complete")?;
            println!("processed {count} local Association(s); Server catalog and profile retained");
        }
        (Some("remove" | "remove-complete" | "repair"), 3) => {
            if !options.confirm {
                return Err(
                    LibraryError::Invalid("this operation requires --confirm".into()).into(),
                );
            }
            let item = library.installation(&args[2])?;
            if &item.profile_id != profile {
                return Err(LibraryError::Invalid(
                    "installation belongs to another profile".into(),
                )
                .into());
            }
            let action = args[1].as_str();
            if action == "repair" {
                let (repaired, quarantine) = library.repair_installation(&item.id)?;
                if options.json {
                    print_json(&repaired)?
                } else {
                    if let Some(quarantine) = quarantine {
                        println!(
                            "repaired {}; previous writable files preserved at {}",
                            repaired.id,
                            quarantine.display()
                        );
                    } else {
                        println!("repaired {} from verified local content", repaired.id);
                    }
                }
            } else {
                library.remove_installation(&item.id, action == "remove-complete")?;
                if !options.json {
                    println!("removed local installation {}", item.id);
                }
            }
        }
        _ => return Err(ApiError::Config(usage().into()).into()),
    }
    Ok(())
}

fn run_library(options: &Options) -> Result<(), CliError> {
    match (
        options.positional.get(1).map(String::as_str),
        options.positional.len(),
    ) {
        (Some("info"), 2) => {
            let library = open_library(options)?;
            println!(
                "Library ID: {}\nMetadata: {}\nContent: {}",
                library.id,
                library.paths.metadata.display(),
                library.paths.content.display()
            );
        }
        (Some("usage"), 2) => {
            let library = open_library(options)?;
            let usage = library.storage_usage()?;
            if options.json {
                print_json(&usage)?
            } else {
                println!(
                    "Immutable objects: {} logical bytes\nVerified materializations: {} logical bytes\nWritable applications: {} logical bytes\nManaged user data: {} logical bytes\nStaging: {} logical bytes\nArtwork: {} logical bytes",
                    usage.immutable_objects_logical_bytes,
                    usage.verified_materializations_logical_bytes,
                    usage.writable_installations_logical_bytes,
                    usage.managed_userdata_logical_bytes,
                    usage.staging_logical_bytes,
                    usage.artwork_logical_bytes
                );
                println!(
                    "Physical reclaimable space may differ because of reflinks, compression and shared content."
                );
            }
        }
        (Some("gc"), 2) => {
            if !options.confirm {
                return Err(LibraryError::Invalid(
                    "immutable-object cleanup requires --confirm".into(),
                )
                .into());
            }
            let mut library = open_library(options)?;
            let bytes = library.collect_unreferenced_objects()?;
            println!(
                "Removed {bytes} logical bytes of unreferenced verified objects; physical free space may differ."
            );
        }
        (Some("migration-preview"), 3) => {
            let source = selected_library_paths(options)?;
            let destination = PathBuf::from(&options.positional[2]);
            let estimate = app_depot_core::migration::migration_estimate(&source, &destination)?;
            if options.json {
                print_json(&estimate)?
            } else {
                println!(
                    "Source logical bytes: {}\nDestination free bytes: {}\nDestination: {}",
                    estimate.source_logical_bytes,
                    estimate.destination_available_bytes,
                    estimate.destination.display()
                );
                println!(
                    "Copy-on-write sharing may reduce the actual space needed; the source stays intact until you inspect the migrated copy."
                );
            }
        }
        (Some("migrate"), 3) => {
            if !options.confirm {
                return Err(LibraryError::Invalid(
                    "migration requires --confirm; the source is preserved".into(),
                )
                .into());
            }
            let source = selected_library_paths(options)?;
            let destination = PathBuf::from(&options.positional[2]);
            let result = app_depot_core::migration::migrate_library(&source, &destination)?;
            println!(
                "Migrated library to {}. The source remains untouched; inspect the destination before cleaning up the old root.",
                result.display()
            );
        }
        _ => return Err(ApiError::Config(usage().into()).into()),
    }
    Ok(())
}

fn run() -> Result<(), CliError> {
    let options = parse_args()?;
    #[cfg(all(feature = "decky", target_os = "linux"))]
    if options.positional[0] == "decky" {
        return decky::run_decky(&options);
    }
    if options.positional[0] == "profile" {
        return run_profile(&options);
    }
    if options.positional[0] == "download" {
        return run_download(&options);
    }
    if options.positional[0] == "launch" {
        return run_launch(&options);
    }
    #[cfg(all(feature = "decky", target_os = "linux"))]
    if options.positional[0] == "decky-prepare" {
        return run_first_play(&options, true);
    }
    if options.positional[0] == "first-play" {
        return run_first_play(&options, false);
    }
    if options.positional[0] == "runtime" {
        return run_runtime(&options);
    }
    if options.positional[0] == "library" {
        return run_library(&options);
    }
    if let Some(profile_id) = &options.profile
        && options.positional[0].starts_with("cached-")
    {
        return run_cached(&options, profile_id);
    }
    let mut library = if options.profile.is_some() {
        Some(open_library(&options)?)
    } else {
        None
    };
    let (server, token) = if let Some(profile_id) = &options.profile {
        let profile = library.as_ref().unwrap().profile(profile_id)?;
        if profile.archived {
            return Err(LibraryError::Invalid("archived profile is offline-only".into()).into());
        }
        let token = selected_token(&profile)?;
        (profile.origin, token)
    } else {
        (
            options
                .server
                .clone()
                .ok_or_else(|| ApiError::Config(format!("server URL required\n{}", usage())))?,
            env::var("APP_DEPOT_TOKEN")
                .ok()
                .filter(|value| !value.is_empty()),
        )
    };
    let client = ApiClient::new(&server, token, options.allow_insecure_http)?;
    if options.positional[0] == "sync" && options.positional.len() == 1 {
        let id = options
            .profile
            .as_ref()
            .ok_or_else(|| ApiError::Config("sync requires --profile ID".into()))?;
        let result = library.as_mut().unwrap().sync(id, &client)?;
        if options.json {
            print_json(&result)?;
        } else {
            println!(
                "synced {} Apps, {} Releases, {} associations, {} Artifacts at {}",
                result.apps, result.releases, result.associations, result.artifacts, result.at
            );
        }
        return Ok(());
    }
    let command = options.positional[0].as_str();
    let needed = match command {
        "capabilities" | "apps" => 1,
        "app" | "releases" | "release" | "artifacts" | "declarations" => 2,
        _ => return Err(ApiError::Config(usage().into()).into()),
    };
    if options.positional.len() != needed {
        return Err(ApiError::Config(usage().into()).into());
    }
    let caps = match client.capabilities() {
        Ok(caps) => caps,
        Err(error) => {
            if let (Some(library), Some(id)) = (&mut library, &options.profile) {
                library.connection_result(id, None, api_connection_status(&error))?;
            }
            return Err(error.into());
        }
    };
    let read_page_size = caps.read_page_size();
    if let (Some(library), Some(id)) = (&mut library, &options.profile) {
        library.connection_result(
            id,
            Some(&caps),
            if read_page_size.is_ok() {
                "connected"
            } else {
                "incompatible"
            },
        )?;
    }
    if command == "capabilities" {
        if options.json {
            print_json(&caps)?;
        } else {
            print_capabilities(&caps);
        }
        let issues = caps.incompatibilities();
        if !issues.is_empty() {
            return Err(ApiError::Incompatible(issues.join("; ")).into());
        }
        return Ok(());
    }
    let page_size = read_page_size.map_err(ApiError::Incompatible)?;
    let id = options.positional.get(1).map(String::as_str).unwrap_or("");
    match command {
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
    .map_err(Into::into)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(match &error {
                CliError::Api(error) | CliError::Library(LibraryError::Api(error)) => {
                    api_exit(error)
                }
                CliError::Library(_) => 9,
                CliError::Secret(_) => 10,
                #[cfg(all(feature = "decky", target_os = "linux"))]
                CliError::Child(status) => {
                    use std::os::unix::process::ExitStatusExt;
                    status
                        .code()
                        .or_else(|| status.signal().map(|signal| 128 + signal))
                        .and_then(|code| u8::try_from(code).ok())
                        .unwrap_or(1)
                }
            })
        }
    }
}
