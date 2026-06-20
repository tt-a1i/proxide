use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::Response,
    routing::{get, post},
    Router,
};
use base64::Engine;
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use walkdir::{DirEntry, WalkDir};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_WRITE_BYTES: usize = 1024 * 1024;
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_PR_BODY_BYTES: usize = 64 * 1024;
const MAX_DIFF_BYTES: usize = 64 * 1024;
const MAX_SHELL_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_LIST_ENTRIES: usize = 500;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_SEARCH_FILES: usize = 5000;
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
const MAX_OPEN_WORKSPACES: usize = 64;
const MAX_INITIALIZED_SESSIONS: usize = 1024;
const MAX_AUDIT_EVENTS_LIMIT: usize = 1000;
const MAX_AUDIT_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_REVIEW_NOTES_LOG_BYTES: u64 = 5 * 1024 * 1024;
const MAX_REVIEW_NOTE_LINE_BYTES: usize = MAX_REVIEW_NOTE_BODY_BYTES + 8 * 1024;
const MAX_INSTRUCTION_BYTES: usize = 16 * 1024;
const MAX_AVAILABLE_INSTRUCTIONS: usize = 200;
const MAX_INSTRUCTION_SCAN_ENTRIES: usize = 5000;
const MAX_REVIEW_NOTE_BODY_BYTES: usize = 32 * 1024;
const MAX_EDIT_PLAN_INTENT_BYTES: usize = 8 * 1024;
const MAX_EDIT_PLAN_PATHS: usize = 50;
const MAX_SKILLS: usize = 100;
const MAX_WORKTREES: usize = 128;
const MAX_PERSISTED_SESSIONS: usize = 256;
const MAX_PERSISTED_PULL_REQUESTS: usize = 256;
const MAX_PERSISTED_EDIT_PLANS: usize = 256;
const MAX_PULL_REQUEST_REFRESHES: usize = 5;
const MAX_REVIEW_NOTES: usize = 1000;
const MAX_SESSION_TOOL_CALLS: usize = 200;
const MAX_SESSION_WORKSPACES: usize = 128;
const MAX_RECENT_CHANGE_ACTIONS: usize = 25;
const SEARCH_DEADLINE: Duration = Duration::from_secs(5);
const INSTRUCTION_SCAN_DEADLINE: Duration = Duration::from_secs(2);
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_PUSH_TIMEOUT: Duration = Duration::from_secs(60);
const GH_TIMEOUT: Duration = Duration::from_secs(60);
const GH_BATCH_TIMEOUT: Duration = Duration::from_secs(90);
const SHELL_TIMEOUT: Duration = Duration::from_secs(10);
const PRIMARY_ENDPOINT: &str = "/mcp";
const LEGACY_ENDPOINT: &str = "/rpc";
const CHANGES_WIDGET_URI: &str = "ui://codex-web-bridge/changes.html";
const REVIEW_WIDGET_URI: &str = "ui://codex-web-bridge/review.html";
const PULL_REQUESTS_WIDGET_URI: &str = "ui://codex-web-bridge/pull-requests.html";
const EDIT_PLANS_WIDGET_URI: &str = "ui://codex-web-bridge/edit-plans.html";
const PROTECTED_RESOURCE_METADATA_ENDPOINT: &str = "/.well-known/oauth-protected-resource";
const PROTECTED_RESOURCE_METADATA_MCP_ENDPOINT: &str = "/.well-known/oauth-protected-resource/mcp";
const AUTHORIZATION_SERVER_METADATA_ENDPOINT: &str = "/.well-known/oauth-authorization-server";
const OAUTH_REGISTER_ENDPOINT: &str = "/oauth/register";
const OAUTH_AUTHORIZE_ENDPOINT: &str = "/oauth/authorize";
const OAUTH_APPROVE_ENDPOINT: &str = "/oauth/approve";
const OAUTH_TOKEN_ENDPOINT: &str = "/oauth/token";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const OAUTH_AUTH_CODE_TTL_MS: u128 = 5 * 60 * 1000;
const OAUTH_ACCESS_TOKEN_TTL_MS: u128 = 60 * 60 * 1000;
const OAUTH_REFRESH_TOKEN_TTL_MS: u128 = 30 * 24 * 60 * 60 * 1000;

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".mypy_cache",
    ".pytest_cache",
    ".venv",
    "node_modules",
    "dist",
    "build",
    ".cache",
    "__pycache__",
    "target",
    "venv",
    "vendor",
];

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum TrustLevel {
    Readonly,
    Review,
    Execute,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawConfig {
    allowed_roots: Vec<PathBuf>,
    #[serde(default)]
    skill_roots: Vec<PathBuf>,
    #[serde(default = "default_trust")]
    trust_level: TrustLevel,
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    owner_token: Option<String>,
    #[serde(default)]
    public_base_url: Option<String>,
    #[serde(default)]
    state_dir: Option<PathBuf>,
    #[serde(default = "default_auto_skill_roots")]
    auto_skill_roots: bool,
}

#[derive(Debug, Clone)]
struct Config {
    allowed_roots: Vec<PathBuf>,
    skill_roots: Vec<PathBuf>,
    auto_skill_roots: bool,
    trust_level: TrustLevel,
    host: String,
    port: u16,
    owner_token: Option<String>,
    public_base_url: Option<String>,
    state_dir: Option<PathBuf>,
}

#[derive(Parser)]
#[command(name = "codex-connector")]
#[command(about = "Rust MCP connector for codex-web-bridge")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    Init {
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
        #[arg(long = "root")]
        roots: Vec<PathBuf>,
        #[arg(long, value_enum, default_value = "readonly")]
        trust_level: TrustLevel,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8765)]
        port: u16,
        #[arg(long)]
        owner_token: Option<String>,
        #[arg(long)]
        no_owner_token: bool,
        #[arg(long)]
        public_base_url: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long = "skill-root")]
        skill_roots: Vec<PathBuf>,
        #[arg(long)]
        no_auto_skill_roots: bool,
        #[arg(long)]
        no_interactive: bool,
        #[arg(long)]
        force: bool,
    },
    Serve {
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
    },
    Doctor {
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
    },
    Audit {
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },
    Worktrees {
        #[command(subcommand)]
        command: WorktreesCommand,
    },
}

#[derive(Subcommand)]
enum SessionsCommand {
    List {
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Show {
        session_id: String,
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum WorktreesCommand {
    List {
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
    },
    Cleanup {
        #[arg(long, default_value = "connector-rs/connector.local.json")]
        config: PathBuf,
    },
}

#[derive(Debug, Clone)]
struct Workspace {
    root: PathBuf,
    activated_skill_dirs: HashSet<PathBuf>,
}

struct InstructionScan {
    loaded: Vec<Value>,
    available: Vec<Value>,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct SkillEntry {
    id: String,
    name: String,
    description: String,
    path: String,
    entrypoint: String,
    dir: PathBuf,
}

struct ReadTarget {
    display_path: String,
    absolute_path: PathBuf,
    activate_skill_dir: Option<PathBuf>,
}

#[derive(Default)]
struct WorkspaceRegistry {
    workspaces: HashMap<String, Workspace>,
    order: VecDeque<String>,
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    registry: Arc<Mutex<WorkspaceRegistry>>,
    initialized_sessions: Arc<Mutex<InitializedSessions>>,
    oauth: Option<Arc<OAuthRuntime>>,
    persisted_state: Option<Arc<Mutex<PersistedState>>>,
}

#[derive(Default)]
struct InitializedSessions {
    set: HashSet<String>,
    order: VecDeque<String>,
}

struct OAuthRuntime {
    owner_secret: OAuthOwnerSecret,
    tokens_path: PathBuf,
    tokens: Mutex<OAuthTokenStore>,
    auth_codes: Mutex<HashMap<String, PendingAuthorization>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthOwnerSecret {
    owner_password: String,
    created_unix_ms: u128,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct OAuthTokenStore {
    #[serde(default)]
    tokens: Vec<StoredOAuthToken>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredOAuthToken {
    access_token: String,
    refresh_token: String,
    client_id: String,
    scope: String,
    resource: Option<String>,
    issued_at_unix_ms: u128,
    expires_at_unix_ms: u128,
    refresh_expires_at_unix_ms: u128,
}

#[derive(Debug, Clone)]
struct PendingAuthorization {
    client_id: String,
    redirect_uri: String,
    scope: String,
    resource: Option<String>,
    code_challenge: String,
    code_challenge_method: String,
    expires_at_unix_ms: u128,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    sessions: Vec<PersistedSession>,
    #[serde(default)]
    pull_requests: Vec<PersistedPullRequest>,
    #[serde(default)]
    edit_plans: Vec<PersistedEditPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSession {
    session_id: String,
    created_unix_ms: u128,
    last_unix_ms: u128,
    initialized: bool,
    #[serde(default)]
    workspaces: Vec<PersistedWorkspace>,
    #[serde(default)]
    tool_calls: Vec<PersistedToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedWorkspace {
    workspace_id: String,
    name: String,
    kind: String,
    opened_unix_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedToolCall {
    ts_unix_ms: u128,
    tool: String,
    outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPullRequest {
    created_unix_ms: u128,
    #[serde(default)]
    updated_unix_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    workspace_id: String,
    branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    title: String,
    #[serde(default)]
    draft: bool,
    #[serde(default = "default_pull_request_status")]
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    merged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(default)]
    body_chars: usize,
}

struct PullRequestRecordKey {
    created_unix_ms: u128,
    branch: String,
    url: Option<String>,
}

struct PullRequestRefreshTarget<'a> {
    selector: String,
    requested_branch: Option<String>,
    requested_url: Option<String>,
    record_key: Option<&'a PullRequestRecordKey>,
}

fn default_pull_request_status() -> String {
    "unknown".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedEditPlan {
    created_unix_ms: u128,
    #[serde(default)]
    updated_unix_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    workspace_id: String,
    plan_id: String,
    #[serde(default = "default_edit_plan_status")]
    status: String,
    title: String,
    intent: String,
    paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch_chars: Option<usize>,
    #[serde(default)]
    files: Vec<PersistedPatchFileSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approved_unix_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied_unix_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied_session_id: Option<String>,
    #[serde(default)]
    applied_files: Vec<PersistedPatchFileSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedPatchFileSummary {
    path: String,
    operation: String,
    bytes_before: usize,
    bytes_after: usize,
}

fn default_edit_plan_status() -> String {
    "draft".to_string()
}

struct WorkspaceOpenRecord<'a> {
    workspace_id: &'a str,
    name: &'a str,
    kind: &'a str,
    source_workspace_id: Option<&'a str>,
    branch: Option<&'a str>,
    base_ref: Option<&'a str>,
    task_id: Option<&'a str>,
    task: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManagedWorktreeMetadata {
    created_unix_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<String>,
}

#[derive(Debug)]
struct ParsedPatchFile {
    path: String,
    operation: PatchOperation,
    hunks: Vec<ParsedHunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchOperation {
    Add,
    Modify,
    Delete,
}

struct PreparedPatchFile {
    path: String,
    target: PathBuf,
    before: String,
    after: Option<String>,
    operation: PatchOperation,
}

struct GitCommandOutput {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    truncated: bool,
}

#[derive(Debug)]
struct ParsedHunk {
    old_start: usize,
    lines: Vec<PatchLine>,
}

#[derive(Debug)]
enum PatchLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::Init {
            config,
            roots,
            trust_level,
            host,
            port,
            owner_token,
            no_owner_token,
            public_base_url,
            state_dir,
            skill_roots,
            no_auto_skill_roots,
            no_interactive,
            force,
        } => cmd_init(InitOptions {
            config_path: config,
            roots,
            trust_level,
            host,
            port,
            owner_token,
            no_owner_token,
            public_base_url,
            state_dir,
            skill_roots,
            auto_skill_roots: !no_auto_skill_roots,
            no_interactive,
            force,
        }),
        CommandKind::Serve { config } => serve(load_config(&config)?).await,
        CommandKind::Doctor { config } => cmd_doctor(config),
        CommandKind::Audit { config, limit } => cmd_audit(config, limit),
        CommandKind::Sessions { command } => match command {
            SessionsCommand::List { config, limit } => cmd_sessions_list(config, limit),
            SessionsCommand::Show {
                session_id,
                config,
                limit,
            } => cmd_sessions_show(config, &session_id, limit),
        },
        CommandKind::Worktrees { command } => match command {
            WorktreesCommand::List { config } => cmd_worktrees_list(config),
            WorktreesCommand::Cleanup { config } => cmd_worktrees_cleanup(config),
        },
    }
}

fn default_trust() -> TrustLevel {
    TrustLevel::Readonly
}

fn default_auto_skill_roots() -> bool {
    true
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8765
}

struct InitOptions {
    config_path: PathBuf,
    roots: Vec<PathBuf>,
    trust_level: TrustLevel,
    host: String,
    port: u16,
    owner_token: Option<String>,
    no_owner_token: bool,
    public_base_url: Option<String>,
    state_dir: Option<PathBuf>,
    skill_roots: Vec<PathBuf>,
    auto_skill_roots: bool,
    no_interactive: bool,
    force: bool,
}

fn cmd_init(options: InitOptions) -> Result<()> {
    let options = maybe_prompt_init_options(options)?;
    write_init_config(options)
}

fn maybe_prompt_init_options(options: InitOptions) -> Result<InitOptions> {
    if !options.roots.is_empty() || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(options);
    }
    if options.no_interactive {
        return Ok(options);
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    prompt_init_options(options, &mut input, &mut output)
}

fn prompt_init_options(
    mut options: InitOptions,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<InitOptions> {
    writeln!(output, "Codex connector setup")?;
    writeln!(output, "Press Enter to accept the value in brackets.")?;
    writeln!(output)?;

    let default_roots = default_allowed_roots_for_prompt()?;
    let default_root_prompt = format_path_list(&default_roots);
    let roots_answer = prompt_line(
        input,
        output,
        "Allowed project roots, comma-separated",
        (!default_root_prompt.is_empty()).then_some(default_root_prompt.as_str()),
    )?;
    options.roots = parse_path_list_with_default(&roots_answer, &default_roots)?;

    let default_port = options.port.to_string();
    let port_answer = prompt_line(input, output, "Local MCP port", Some(&default_port))?;
    options.port = port_answer
        .trim()
        .parse::<u16>()
        .with_context(|| "local MCP port must be a number between 0 and 65535")?;

    if options.public_base_url.is_none() {
        let public_answer = prompt_line(
            input,
            output,
            "Public HTTPS origin for ChatGPT, without /mcp (optional)",
            None,
        )?;
        let public_answer = public_answer.trim().trim_end_matches('/').to_string();
        if !public_answer.is_empty() {
            options.public_base_url = Some(public_answer);
        }
    }

    if options.skill_roots.is_empty() {
        let default_skill_roots = default_skill_roots_for_prompt();
        let default_skill_prompt = format_path_list(&default_skill_roots);
        let skill_answer = prompt_line(
            input,
            output,
            "Connector package skill roots, comma-separated (optional; type none to skip all skill discovery)",
            (!default_skill_prompt.is_empty()).then_some(default_skill_prompt.as_str()),
        )?;
        if skill_answer.trim().eq_ignore_ascii_case("none") {
            options.auto_skill_roots = false;
        }
        options.skill_roots =
            parse_optional_path_list_with_default(&skill_answer, &default_skill_roots)?;
    }

    writeln!(output)?;
    writeln!(
        output,
        "Writing {:?} config. Use --trust-level execute only when you intentionally want mutating tools.",
        options.trust_level
    )?;
    output.flush()?;
    Ok(options)
}

fn prompt_line(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    match default {
        Some(default) => write!(output, "{label} [{default}]: ")?,
        None => write!(output, "{label}: ")?,
    }
    output.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        Ok(default.unwrap_or("").to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn parse_path_list_with_default(input: &str, default: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if input.trim().is_empty() {
        if default.is_empty() {
            bail!("path list must include at least one path");
        }
        return Ok(default.to_vec());
    }
    let paths = input
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        bail!("path list must include at least one path");
    }
    Ok(paths)
}

fn default_allowed_roots_for_prompt() -> Result<Vec<PathBuf>> {
    let cwd = env::current_dir()?;
    if is_broad_root(&cwd) {
        Ok(vec![])
    } else {
        Ok(vec![cwd])
    }
}

fn parse_optional_path_list_with_default(input: &str, default: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if input.trim().is_empty() && default.is_empty() {
        return Ok(vec![]);
    }
    if input.trim().eq_ignore_ascii_case("none") {
        return Ok(vec![]);
    }
    parse_path_list_with_default(input, default)
}

fn default_skill_roots_for_prompt() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        push_skill_root_candidate(&mut roots, cwd.join("skills"));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(package_root) = exe.parent().and_then(Path::parent) {
            push_skill_root_candidate(&mut roots, package_root.join("skills"));
        }
    }
    roots
}

fn push_skill_root_candidate(roots: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !candidate.is_dir() {
        return;
    }
    if roots.iter().any(|root| root == &candidate) {
        return;
    }
    roots.push(candidate);
}

fn format_path_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn write_init_config(options: InitOptions) -> Result<()> {
    let InitOptions {
        config_path,
        roots,
        trust_level,
        host,
        port,
        owner_token,
        no_owner_token,
        public_base_url,
        state_dir,
        skill_roots,
        auto_skill_roots,
        no_interactive: _,
        force,
    } = options;
    if config_path.exists() && !force {
        bail!(
            "config already exists: {} (use --force)",
            config_path.display()
        );
    }
    if owner_token.is_some() && no_owner_token {
        bail!("--owner-token and --no-owner-token cannot be used together");
    }
    let roots = if roots.is_empty() {
        vec![env::current_dir()?]
    } else {
        roots
    };
    let owner_token = match (owner_token, no_owner_token) {
        (Some(token), false) => Some(token),
        (None, false) => Some(format!(
            "{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        )),
        (None, true) => None,
        (Some(_), true) => unreachable!(),
    };
    let raw = RawConfig {
        allowed_roots: roots,
        skill_roots,
        trust_level,
        host,
        port,
        owner_token,
        public_base_url: public_base_url.map(|url| url.trim_end_matches('/').to_string()),
        state_dir: state_dir.or_else(default_state_dir),
        auto_skill_roots,
    };
    let config = validate_raw(raw)?;
    let raw = RawConfig {
        allowed_roots: config.allowed_roots.clone(),
        skill_roots: config.skill_roots.clone(),
        auto_skill_roots: config.auto_skill_roots,
        trust_level: config.trust_level,
        host: config.host.clone(),
        port: config.port,
        owner_token: config.owner_token.clone(),
        public_base_url: config.public_base_url.clone(),
        state_dir: config.state_dir.clone(),
    };
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&config_path, serde_json::to_string_pretty(&raw)? + "\n")?;
    println!("wrote {}", config_path.display());
    println!(
        "local endpoint: http://{}:{}{}",
        config.host, config.port, PRIMARY_ENDPOINT
    );
    if let Some(public) = &config.public_base_url {
        println!("external endpoint: {}{}", public, PRIMARY_ENDPOINT);
    }
    if config.owner_token.is_some() {
        println!("owner token: configured");
    } else {
        println!("owner token: not configured; use only for temporary no-auth smoke tests");
    }
    if let Some(state_dir) = &config.state_dir {
        println!("state dir: {}", state_dir.display());
        let (oauth_owner, created) = ensure_oauth_owner_secret(state_dir)?;
        if created {
            println!("owner approval password: {}", oauth_owner.owner_password);
            println!(
                "owner approval password file: {}",
                state_dir.join("oauth_owner.local.json").display()
            );
        } else {
            println!(
                "owner approval password: already configured in {}",
                state_dir.join("oauth_owner.local.json").display()
            );
        }
    }
    Ok(())
}

fn cmd_doctor(config_path: PathBuf) -> Result<()> {
    let config = load_config(&config_path)?;
    println!("status: ok");
    println!("config: {}", config_path.display());
    println!("trust_level: {:?}", config.trust_level);
    println!(
        "local_endpoint: http://{}:{}{}",
        config.host, config.port, PRIMARY_ENDPOINT
    );
    if let Some(public) = &config.public_base_url {
        println!("external_endpoint: {}{}", public, PRIMARY_ENDPOINT);
    } else {
        println!("external_endpoint: not set");
    }
    println!(
        "auth: {}",
        if config.owner_token.is_some() {
            "owner token configured"
        } else {
            "no owner token"
        }
    );
    println!(
        "state_dir: {}",
        config
            .state_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not set; audit disabled".to_string())
    );
    if let Some(state_dir) = &config.state_dir {
        let (_, created) = ensure_oauth_owner_secret(state_dir)?;
        println!(
            "oauth_owner_secret: {}",
            if created { "created" } else { "configured" }
        );
        println!(
            "oauth_metadata: {}, {}",
            public_url(&config, PROTECTED_RESOURCE_METADATA_ENDPOINT),
            public_url(&config, AUTHORIZATION_SERVER_METADATA_ENDPOINT)
        );
        println!(
            "workspace_state: {}",
            state_snapshot_path(state_dir).display()
        );
        println!(
            "host_allowlist: {}",
            config
                .public_base_url
                .as_ref()
                .and_then(|url| url::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(ToString::to_string))
                .unwrap_or_else(|| "loopback/local only".to_string())
        );
    } else {
        println!("oauth_owner_secret: state_dir not configured");
        println!("oauth_metadata: unavailable without state_dir");
    }
    println!(
        "skill_roots: {}",
        if config.skill_roots.is_empty() {
            "not set".to_string()
        } else {
            config
                .skill_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!(
        "auto_skill_roots: {}",
        if config.auto_skill_roots {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!("tools: {}", tool_names(config.trust_level).join(", "));
    println!("git: {}", if git_available() { "ok" } else { "not found" });
    println!("gh: {}", if gh_available() { "ok" } else { "not found" });
    println!(
        "git_worktree: {}",
        if git_worktree_available(&config.allowed_roots) {
            "ok"
        } else {
            "not available for configured roots"
        }
    );
    println!(
        "bash: {}",
        if bash_available() { "ok" } else { "not found" }
    );
    Ok(())
}

fn cmd_audit(config_path: PathBuf, limit: usize) -> Result<()> {
    let config = load_config(&config_path)?;
    let Some(state_dir) = config.state_dir else {
        bail!("state_dir is not configured; audit logging is disabled");
    };
    let events = read_audit_events(&state_dir, limit)?;
    if events.is_empty() {
        println!("no audit events");
        return Ok(());
    }
    for event in events {
        let ts = event.get("ts_unix_ms").and_then(Value::as_u64).unwrap_or(0);
        let tool = event.get("tool").and_then(Value::as_str).unwrap_or("-");
        let outcome = event.get("outcome").and_then(Value::as_str).unwrap_or("-");
        println!("{ts} tool={tool} outcome={outcome}");
    }
    Ok(())
}

fn cmd_sessions_list(config_path: PathBuf, limit: usize) -> Result<()> {
    let config = load_config(&config_path)?;
    let Some(state_dir) = config.state_dir else {
        bail!("state_dir is not configured; audit logging is disabled");
    };
    let mut sessions = persisted_session_summaries(&state_dir, limit)?;
    if sessions.is_empty() {
        let events = read_audit_events(&state_dir, limit)?;
        sessions = summarize_sessions(&events);
    }
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    for session in sessions {
        println!(
            "{} calls={} workspaces={} last_ts={} tools={}",
            session["session_id"].as_str().unwrap_or("-"),
            session["call_count"].as_u64().unwrap_or(0),
            session["workspace_count"].as_u64().unwrap_or(0),
            session["last_ts_unix_ms"].as_u64().unwrap_or(0),
            session["tools"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn cmd_sessions_show(config_path: PathBuf, session_id: &str, limit: usize) -> Result<()> {
    let config = load_config(&config_path)?;
    let Some(state_dir) = config.state_dir else {
        bail!("state_dir is not configured; audit logging is disabled");
    };
    if let Some(detail) = persisted_session_detail(&state_dir, session_id)? {
        println!("{}", serde_json::to_string_pretty(&detail)?);
        return Ok(());
    }
    let events = read_audit_events_for_session(&state_dir, session_id, limit)?;
    let detail = session_detail(&events, session_id);
    println!("{}", serde_json::to_string_pretty(&detail)?);
    Ok(())
}

fn cmd_worktrees_list(config_path: PathBuf) -> Result<()> {
    let config = load_config(&config_path)?;
    let Some(state_dir) = config.state_dir else {
        bail!("state_dir is not configured; worktree state is disabled");
    };
    let worktrees = managed_worktrees(&state_dir)?;
    if worktrees.is_empty() {
        println!("no managed worktrees");
        return Ok(());
    }
    for worktree in worktrees {
        println!("{}", worktree.display());
    }
    Ok(())
}

fn cmd_worktrees_cleanup(config_path: PathBuf) -> Result<()> {
    let config = load_config(&config_path)?;
    let Some(state_dir) = config.state_dir else {
        bail!("state_dir is not configured; worktree state is disabled");
    };
    let worktrees = managed_worktrees(&state_dir)?;
    if worktrees.is_empty() {
        println!("no managed worktrees");
        return Ok(());
    }
    let mut removed = 0usize;
    for worktree in worktrees {
        let managed_name = worktree
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToString::to_string);
        if remove_managed_worktree(&worktree).is_ok() {
            if let Some(managed_name) = managed_name {
                let _ = fs::remove_file(worktree_metadata_path(&state_dir, &managed_name));
            }
            removed += 1;
        }
    }
    println!("removed {removed} managed worktrees");
    Ok(())
}

fn load_config(path: &Path) -> Result<Config> {
    let raw: RawConfig = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read config {}", path.display()))?,
    )?;
    validate_raw(raw)
}

fn validate_raw(raw: RawConfig) -> Result<Config> {
    if raw.allowed_roots.is_empty() {
        bail!("allowed_roots must not be empty");
    }
    let mut roots = Vec::new();
    for root in raw.allowed_roots {
        let expanded = expand_home(&root);
        let canonical = expanded
            .canonicalize()
            .with_context(|| format!("allowed root does not exist: {}", expanded.display()))?;
        if is_broad_root(&canonical) {
            bail!("allowed root is too broad: {}", canonical.display());
        }
        if !canonical.is_dir() {
            bail!("allowed root is not a directory: {}", canonical.display());
        }
        roots.push(canonical);
    }
    let mut skill_roots = Vec::new();
    for root in raw.skill_roots {
        let expanded = expand_home(&root);
        let canonical = expanded
            .canonicalize()
            .with_context(|| format!("skill root does not exist: {}", expanded.display()))?;
        if !canonical.is_dir() {
            bail!("skill root is not a directory: {}", canonical.display());
        }
        skill_roots.push(canonical);
    }
    let public_base_url = raw
        .public_base_url
        .map(|url| url.trim_end_matches('/').to_string());
    if let Some(url) = &public_base_url {
        let parsed = url::Url::parse(url).with_context(|| "public_base_url must be a URL")?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            bail!("public_base_url must be an http(s) origin");
        }
        if parsed.scheme() != "https" {
            let host = parsed.host_str().unwrap_or_default();
            if !matches!(host, "127.0.0.1" | "::1" | "localhost") {
                bail!("public_base_url must use https unless it is loopback localhost");
            }
        }
        if parsed.path() != "/" && !parsed.path().is_empty() {
            bail!("public_base_url must be the public origin without /mcp or any path");
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("public_base_url must not include query or fragment");
        }
    }
    if !is_loopback(&raw.host) && raw.owner_token.is_none() {
        bail!("owner_token is required when binding to a non-loopback host");
    }
    if raw.trust_level == TrustLevel::Execute && raw.owner_token.is_none() {
        bail!("owner_token is required when trust_level=execute");
    }
    let state_dir = raw
        .state_dir
        .map(|path| absolutize(&expand_home(&path)))
        .transpose()?;
    Ok(Config {
        allowed_roots: roots,
        skill_roots,
        auto_skill_roots: raw.auto_skill_roots,
        trust_level: raw.trust_level,
        host: raw.host,
        port: raw.port,
        owner_token: raw.owner_token,
        public_base_url,
        state_dir,
    })
}

fn default_state_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/codex-web-bridge/connector-rs"))
}

async fn serve(config: Config) -> Result<()> {
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| "host must be an IP address such as 127.0.0.1")?;
    let state = build_state(config)?;
    let app = build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "[connector-rs] trust={:?} MCP listening on http://{}:{}{}",
        state.config.trust_level, state.config.host, state.config.port, PRIMARY_ENDPOINT
    );
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_state(config: Config) -> Result<AppState> {
    let oauth = match &config.state_dir {
        Some(state_dir) => Some(Arc::new(load_oauth_runtime(state_dir)?)),
        None => None,
    };
    let persisted_state = match &config.state_dir {
        Some(state_dir) => Some(Arc::new(Mutex::new(load_persisted_state(state_dir)?))),
        None => None,
    };
    Ok(AppState {
        config: Arc::new(config),
        registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
        initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
        oauth,
        persisted_state,
    })
}

fn state_snapshot_path(state_dir: &Path) -> PathBuf {
    state_dir.join("workspace_state.json")
}

fn load_persisted_state(state_dir: &Path) -> Result<PersistedState> {
    let path = state_snapshot_path(state_dir);
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let mut state: PersistedState = serde_json::from_slice(&fs::read(path)?)?;
    cap_persisted_state(&mut state);
    Ok(state)
}

fn persist_state_snapshot(state_dir: &Path, state: &PersistedState) -> Result<()> {
    write_private_json(&state_snapshot_path(state_dir), state)
}

fn load_oauth_runtime(state_dir: &Path) -> Result<OAuthRuntime> {
    fs::create_dir_all(state_dir)?;
    let (owner_secret, _) = ensure_oauth_owner_secret(state_dir)?;
    let tokens_path = state_dir.join("oauth_tokens.json");
    let tokens = if tokens_path.exists() {
        serde_json::from_slice(&fs::read(&tokens_path)?)?
    } else {
        OAuthTokenStore::default()
    };
    Ok(OAuthRuntime {
        owner_secret,
        tokens_path,
        tokens: Mutex::new(tokens),
        auth_codes: Mutex::new(HashMap::new()),
    })
}

fn ensure_oauth_owner_secret(state_dir: &Path) -> Result<(OAuthOwnerSecret, bool)> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join("oauth_owner.local.json");
    if path.exists() {
        let secret: OAuthOwnerSecret = serde_json::from_slice(&fs::read(path)?)?;
        return Ok((secret, false));
    }
    let secret = OAuthOwnerSecret {
        owner_password: random_secret(),
        created_unix_ms: unix_ms(),
    };
    write_private_json(&path, &secret)?;
    Ok((secret, true))
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(value)? + "\n";
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, data)?;
        Ok(())
    }
}

fn persist_oauth_tokens(oauth: &OAuthRuntime, store: &OAuthTokenStore) -> Result<()> {
    write_private_json(&oauth.tokens_path, store)
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn exchange_authorization_code(oauth: &OAuthRuntime, form: &HashMap<String, String>) -> Response {
    let code = form.get("code").map(String::as_str).unwrap_or("");
    let client_id = form.get("client_id").map(String::as_str).unwrap_or("");
    let redirect_uri = form.get("redirect_uri").map(String::as_str).unwrap_or("");
    let verifier = form.get("code_verifier").map(String::as_str).unwrap_or("");
    if code.is_empty() || client_id.is_empty() || redirect_uri.is_empty() || verifier.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing required parameter",
        );
    }
    let pending = {
        let mut codes = oauth.auth_codes.lock().unwrap();
        codes.remove(code)
    };
    let Some(pending) = pending else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "authorization code is invalid",
        );
    };
    if pending.expires_at_unix_ms < unix_ms() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "authorization code expired",
        );
    }
    if pending.client_id != client_id || pending.redirect_uri != redirect_uri {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "authorization request mismatch",
        );
    }
    if !pkce_ok(
        &pending.code_challenge,
        &pending.code_challenge_method,
        verifier,
    ) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "PKCE verifier mismatch",
        );
    }
    issue_token_response(oauth, client_id, &pending.scope, pending.resource)
}

fn refresh_access_token(oauth: &OAuthRuntime, form: &HashMap<String, String>) -> Response {
    let refresh_token = form.get("refresh_token").map(String::as_str).unwrap_or("");
    let client_id = form.get("client_id").map(String::as_str).unwrap_or("");
    if refresh_token.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing refresh_token",
        );
    }
    let now = unix_ms();
    let mut store = oauth.tokens.lock().unwrap();
    prune_expired_tokens(&mut store, now);
    let Some(existing) = store
        .tokens
        .iter()
        .find(|token| {
            constant_time_eq(&token.refresh_token, refresh_token)
                && (client_id.is_empty() || token.client_id == client_id)
                && token.refresh_expires_at_unix_ms >= now
        })
        .cloned()
    else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "refresh token is invalid",
        );
    };
    let access_token = random_secret();
    let issued_at = unix_ms();
    let mut updated = existing.clone();
    updated.access_token = access_token.clone();
    updated.issued_at_unix_ms = issued_at;
    updated.expires_at_unix_ms = issued_at + OAUTH_ACCESS_TOKEN_TTL_MS;
    store
        .tokens
        .retain(|token| token.refresh_token != existing.refresh_token);
    store.tokens.push(updated.clone());
    if let Err(err) = persist_oauth_tokens(oauth, &store) {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            &err.to_string(),
        );
    }
    token_response(&updated)
}

fn issue_token_response(
    oauth: &OAuthRuntime,
    client_id: &str,
    scope: &str,
    resource: Option<String>,
) -> Response {
    let issued_at = unix_ms();
    let token = StoredOAuthToken {
        access_token: random_secret(),
        refresh_token: random_secret(),
        client_id: client_id.to_string(),
        scope: scope.to_string(),
        resource,
        issued_at_unix_ms: issued_at,
        expires_at_unix_ms: issued_at + OAUTH_ACCESS_TOKEN_TTL_MS,
        refresh_expires_at_unix_ms: issued_at + OAUTH_REFRESH_TOKEN_TTL_MS,
    };
    let mut store = oauth.tokens.lock().unwrap();
    prune_expired_tokens(&mut store, issued_at);
    store.tokens.push(token.clone());
    if let Err(err) = persist_oauth_tokens(oauth, &store) {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            &err.to_string(),
        );
    }
    token_response(&token)
}

fn token_response(token: &StoredOAuthToken) -> Response {
    json_response(
        StatusCode::OK,
        json!({
            "access_token": token.access_token,
            "token_type": "Bearer",
            "expires_in": (OAUTH_ACCESS_TOKEN_TTL_MS / 1000) as u64,
            "refresh_token": token.refresh_token,
            "scope": token.scope
        }),
    )
}

fn oauth_access_token_scope(oauth: &OAuthRuntime, presented: &str) -> Option<String> {
    let now = unix_ms();
    let store = oauth.tokens.lock().unwrap();
    store.tokens.iter().find_map(|token| {
        (token.expires_at_unix_ms >= now && constant_time_eq(&token.access_token, presented))
            .then(|| token.scope.clone())
    })
}

fn oauth_access_token_valid(oauth: &OAuthRuntime, presented: &str) -> bool {
    oauth_access_token_scope(oauth, presented).is_some()
}

fn prune_expired_tokens(store: &mut OAuthTokenStore, now: u128) {
    store
        .tokens
        .retain(|token| token.refresh_expires_at_unix_ms >= now);
}

fn pkce_ok(challenge: &str, method: &str, verifier: &str) -> bool {
    match method {
        "plain" => constant_time_eq(challenge, verifier),
        "S256" => {
            let digest = Sha256::digest(verifier.as_bytes());
            let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            constant_time_eq(challenge, &encoded)
        }
        _ => false,
    }
}

fn validate_authorize_query(
    query: &HashMap<String, String>,
    config: &Config,
) -> Result<(), String> {
    if query.get("response_type").map(String::as_str) != Some("code") {
        return Err("response_type must be code".to_string());
    }
    for key in ["client_id", "redirect_uri", "code_challenge"] {
        if query
            .get(key)
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            return Err(format!("{key} is required"));
        }
    }
    let method = query
        .get("code_challenge_method")
        .map(String::as_str)
        .unwrap_or("plain");
    if !matches!(method, "plain" | "S256") {
        return Err("code_challenge_method must be S256 or plain".to_string());
    }
    let redirect = query.get("redirect_uri").map(String::as_str).unwrap_or("");
    match url::Url::parse(redirect) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => {}
        _ => return Err("redirect_uri must be an absolute http(s) URL".to_string()),
    }
    normalize_oauth_scope(config, query.get("scope").map(String::as_str)).map(|_| ())
}

fn parse_query(input: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(input.as_bytes())
        .into_owned()
        .collect()
}

fn append_query_params(base: &str, params: &[(&str, &str)]) -> String {
    let mut url = match url::Url::parse(base) {
        Ok(url) => url,
        Err(_) => return base.to_string(),
    };
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            if !value.is_empty() {
                pairs.append_pair(key, value);
            }
        }
    }
    url.to_string()
}

fn hidden_authorize_inputs(query: &HashMap<String, String>) -> String {
    let mut out = String::new();
    for key in [
        "response_type",
        "client_id",
        "redirect_uri",
        "scope",
        "state",
        "resource",
        "code_challenge",
        "code_challenge_method",
    ] {
        if let Some(value) = query.get(key) {
            out.push_str(&format!(
                r#"<input type="hidden" name="{}" value="{}">"#,
                key,
                html_escape(value)
            ));
        }
    }
    out
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn public_url(config: &Config, path: &str) -> String {
    let origin = config.public_base_url.clone().unwrap_or_else(|| {
        format!(
            "http://{}:{}",
            if config.host == "0.0.0.0" {
                "127.0.0.1"
            } else {
                &config.host
            },
            config.port
        )
    });
    if path.is_empty() {
        origin
    } else {
        format!("{}{}", origin.trim_end_matches('/'), path)
    }
}

fn oauth_scopes(config: &Config) -> Vec<&'static str> {
    match config.trust_level {
        TrustLevel::Execute => vec!["workspace:read", "workspace:write", "shell"],
        _ => vec!["workspace:read"],
    }
}

fn normalize_oauth_scope(config: &Config, scope: Option<&str>) -> Result<String, String> {
    let supported = oauth_scopes(config);
    let requested = scope.unwrap_or("").trim();
    if requested.is_empty() {
        return Ok(supported.join(" "));
    }
    let mut out = Vec::new();
    for item in requested.split_whitespace() {
        if !supported.contains(&item) {
            return Err(format!("unsupported scope: {item}"));
        }
        if !out.contains(&item) {
            out.push(item);
        }
    }
    Ok(out.join(" "))
}

fn scope_includes(scope: &str, required: &str) -> bool {
    scope.split_whitespace().any(|item| item == required)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route(
            PROTECTED_RESOURCE_METADATA_ENDPOINT,
            get(handle_protected_resource_metadata),
        )
        .route(
            PROTECTED_RESOURCE_METADATA_MCP_ENDPOINT,
            get(handle_protected_resource_metadata),
        )
        .route(
            AUTHORIZATION_SERVER_METADATA_ENDPOINT,
            get(handle_authorization_server_metadata),
        )
        .route(OAUTH_REGISTER_ENDPOINT, post(handle_oauth_register))
        .route(OAUTH_AUTHORIZE_ENDPOINT, get(handle_oauth_authorize))
        .route(OAUTH_APPROVE_ENDPOINT, post(handle_oauth_approve))
        .route(OAUTH_TOKEN_ENDPOINT, post(handle_oauth_token))
        .route(
            PRIMARY_ENDPOINT,
            post(handle_rpc)
                .get(method_not_allowed)
                .delete(method_not_allowed),
        )
        .route(
            LEGACY_ENDPOINT,
            post(handle_rpc)
                .get(method_not_allowed)
                .delete(method_not_allowed),
        )
        .fallback(not_found)
        .with_state(state)
}

async fn handle_protected_resource_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !host_ok(&state.config, &headers) {
        return json_response(StatusCode::FORBIDDEN, json!({"error": "forbidden_host"}));
    }
    let resource = public_url(&state.config, PRIMARY_ENDPOINT);
    json_response(
        StatusCode::OK,
        json!({
            "resource": resource,
            "authorization_servers": [public_url(&state.config, "")],
            "bearer_methods_supported": ["header"],
            "scopes_supported": oauth_scopes(&state.config),
            "resource_documentation": public_url(&state.config, "/")
        }),
    )
}

async fn handle_authorization_server_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !host_ok(&state.config, &headers) {
        return json_response(StatusCode::FORBIDDEN, json!({"error": "forbidden_host"}));
    }
    json_response(
        StatusCode::OK,
        json!({
            "issuer": public_url(&state.config, ""),
            "authorization_endpoint": public_url(&state.config, OAUTH_AUTHORIZE_ENDPOINT),
            "token_endpoint": public_url(&state.config, OAUTH_TOKEN_ENDPOINT),
            "registration_endpoint": public_url(&state.config, OAUTH_REGISTER_ENDPOINT),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256", "plain"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": oauth_scopes(&state.config)
        }),
    )
}

async fn handle_oauth_register(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !host_ok(&state.config, &headers) {
        return json_response(StatusCode::FORBIDDEN, json!({"error": "forbidden_host"}));
    }
    if body.len() > MAX_BODY_BYTES {
        return oauth_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "request too large",
        );
    }
    let parsed: Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "invalid json");
            }
        }
    };
    let redirect_uris = parsed
        .get("redirect_uris")
        .cloned()
        .unwrap_or_else(|| json!([]));
    json_response(
        StatusCode::CREATED,
        json!({
            "client_id": format!("codex-chatgpt-{}", Uuid::new_v4().simple()),
            "client_id_issued_at": unix_ms() / 1000,
            "token_endpoint_auth_method": "none",
            "redirect_uris": redirect_uris,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "scope": oauth_scopes(&state.config).join(" ")
        }),
    )
}

async fn handle_oauth_authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !host_ok(&state.config, &headers) {
        return html_response(StatusCode::FORBIDDEN, "forbidden host");
    }
    let Some(oauth) = &state.oauth else {
        return html_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "OAuth state_dir is not configured",
        );
    };
    let query = parse_query(uri.query().unwrap_or(""));
    let validation = validate_authorize_query(&query, &state.config);
    if let Err(message) = validation {
        return html_response(StatusCode::BAD_REQUEST, &message);
    }
    let resource = query.get("resource").cloned().unwrap_or_default();
    let scope = normalize_oauth_scope(&state.config, query.get("scope").map(String::as_str))
        .unwrap_or_else(|_| query.get("scope").cloned().unwrap_or_default());
    let html = format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>Codex Connector Approval</title>
<body style="font-family: system-ui, sans-serif; max-width: 680px; margin: 40px auto; line-height: 1.5;">
<h1>Approve Codex Connector</h1>
<p>Client <code>{}</code> is requesting access to <code>{}</code>.</p>
<p>Scope: <code>{}</code></p>
<form method="post" action="{}">
{}
<label>Owner approval password<br><input name="owner_password" type="password" autocomplete="current-password" autofocus style="width: 100%; padding: 8px;"></label>
<p><button type="submit">Approve</button></p>
</form>
</body>"#,
        html_escape(query.get("client_id").map(String::as_str).unwrap_or("")),
        html_escape(if resource.is_empty() {
            PRIMARY_ENDPOINT
        } else {
            &resource
        }),
        html_escape(if scope.is_empty() { "default" } else { &scope }),
        OAUTH_APPROVE_ENDPOINT,
        hidden_authorize_inputs(&query),
    );
    let _ = oauth;
    html_response(StatusCode::OK, &html)
}

async fn handle_oauth_approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !host_ok(&state.config, &headers) {
        return html_response(StatusCode::FORBIDDEN, "forbidden host");
    }
    let Some(oauth) = &state.oauth else {
        return html_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "OAuth state_dir is not configured",
        );
    };
    if content_type(&headers) != "application/x-www-form-urlencoded" {
        return html_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "content-type must be application/x-www-form-urlencoded",
        );
    }
    if body.len() > MAX_BODY_BYTES {
        return html_response(StatusCode::PAYLOAD_TOO_LARGE, "request too large");
    }
    let form = parse_query(&String::from_utf8_lossy(&body));
    if !constant_time_eq(
        form.get("owner_password").map(String::as_str).unwrap_or(""),
        &oauth.owner_secret.owner_password,
    ) {
        return html_response(StatusCode::UNAUTHORIZED, "invalid owner approval password");
    }
    if let Err(message) = validate_authorize_query(&form, &state.config) {
        return html_response(StatusCode::BAD_REQUEST, &message);
    }
    let scope = match normalize_oauth_scope(&state.config, form.get("scope").map(String::as_str)) {
        Ok(scope) => scope,
        Err(message) => return html_response(StatusCode::BAD_REQUEST, &message),
    };
    let code = random_secret();
    let pending = PendingAuthorization {
        client_id: form.get("client_id").cloned().unwrap_or_default(),
        redirect_uri: form.get("redirect_uri").cloned().unwrap_or_default(),
        scope,
        resource: form.get("resource").cloned(),
        code_challenge: form.get("code_challenge").cloned().unwrap_or_default(),
        code_challenge_method: form
            .get("code_challenge_method")
            .cloned()
            .unwrap_or_else(|| "plain".to_string()),
        expires_at_unix_ms: unix_ms() + OAUTH_AUTH_CODE_TTL_MS,
    };
    oauth
        .auth_codes
        .lock()
        .unwrap()
        .insert(code.clone(), pending);
    let redirect = append_query_params(
        form.get("redirect_uri").map(String::as_str).unwrap_or(""),
        &[
            ("code", code.as_str()),
            ("state", form.get("state").map(String::as_str).unwrap_or("")),
        ],
    );
    redirect_response(&redirect)
}

async fn handle_oauth_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !host_ok(&state.config, &headers) {
        return oauth_error(StatusCode::FORBIDDEN, "invalid_request", "forbidden host");
    }
    let Some(oauth) = &state.oauth else {
        return oauth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "OAuth state_dir is not configured",
        );
    };
    if content_type(&headers) != "application/x-www-form-urlencoded" {
        return oauth_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_request",
            "content-type must be application/x-www-form-urlencoded",
        );
    }
    if body.len() > MAX_BODY_BYTES {
        return oauth_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "request too large",
        );
    }
    let form = parse_query(&String::from_utf8_lossy(&body));
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => exchange_authorization_code(oauth, &form),
        Some("refresh_token") => refresh_access_token(oauth, &form),
        _ => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "unsupported grant_type",
        ),
    }
}

async fn handle_rpc(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !host_ok(&state.config, &headers) {
        return json_rpc_response(
            StatusCode::FORBIDDEN,
            rpc_error(None, -32001, "forbidden host"),
            None,
        );
    }
    if !origin_ok(&headers) {
        return json_rpc_response(
            StatusCode::FORBIDDEN,
            rpc_error(None, -32001, "forbidden origin"),
            None,
        );
    }
    if !authorized(&state, &headers) {
        return unauthorized_rpc_response(&state.config);
    }
    if content_type(&headers) != "application/json" {
        return json_rpc_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            rpc_error(None, -32600, "content-type must be application/json"),
            None,
        );
    }
    if body.len() > MAX_BODY_BYTES {
        return json_rpc_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            rpc_error(None, -32600, "request too large"),
            None,
        );
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return json_rpc_response(
                StatusCode::BAD_REQUEST,
                rpc_error(None, -32700, "parse error"),
                None,
            )
        }
    };
    let Some(obj) = message.as_object() else {
        return json_rpc_response(
            StatusCode::BAD_REQUEST,
            rpc_error(None, -32600, "invalid request"),
            None,
        );
    };
    let req_id = obj.get("id").cloned();
    let method = obj.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        if req_id.is_none() {
            return empty_response(StatusCode::ACCEPTED);
        }
        return json_rpc_response(
            StatusCode::OK,
            rpc_error(req_id, -32600, "method must be a string"),
            None,
        );
    };
    let is_notification = req_id.is_none();
    let mut session_id = header_string(&headers, "mcp-session-id");
    let mut issued_session = None;
    if method == "initialize" {
        let new_id = Uuid::new_v4().simple().to_string();
        register_session(state.initialized_sessions.lock().unwrap(), new_id.clone());
        record_session_initialized(&state, &new_id);
        session_id = Some(new_id.clone());
        issued_session = Some(new_id);
    }
    if is_notification {
        if method == "notifications/initialized" {
            if let Some(sid) = session_id {
                record_session_initialized(&state, &sid);
                register_session(state.initialized_sessions.lock().unwrap(), sid);
            }
        }
        return empty_response(StatusCode::ACCEPTED);
    }
    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
    if method == "tools/call" {
        if let Some(required_scope) = tool_call_name(&params).and_then(required_tool_scope) {
            if !authorized_for_scope(&state, &headers, required_scope) {
                return json_rpc_response(
                    StatusCode::OK,
                    rpc_error(
                        req_id,
                        -32003,
                        &format!("insufficient OAuth scope: {required_scope}"),
                    ),
                    issued_session.as_deref(),
                );
            }
        }
    }
    let response = handle_request(&state, req_id, method, params, session_id);
    json_rpc_response(StatusCode::OK, response, issued_session.as_deref())
}

fn handle_request(
    state: &AppState,
    req_id: Option<Value>,
    method: &str,
    params: Value,
    session_id: Option<String>,
) -> Value {
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                LATEST_PROTOCOL_VERSION
            };
            let instructions = match state.config.trust_level {
                TrustLevel::Execute => "Rust connector in execute mode. Open a workspace inside an allowed root, follow returned project instructions, read relevant nested instruction files before working under those directories, and read a skill's skill://.../SKILL.md entrypoint before reading other files from that skill. Use scoped write/edit tools only when the user intended local code mutation.",
                _ => "Readonly Rust connector. Open a workspace inside an allowed root, follow returned project instructions, read relevant nested instruction files before working under those directories, and read a skill's skill://.../SKILL.md entrypoint before reading other files from that skill.",
            };
            rpc_result(
                req_id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {"listChanged": false},
                        "resources": {"subscribe": false, "listChanged": false}
                    },
                    "serverInfo": {
                        "name": "codex-web-bridge-connector-rs",
                        "title": "Codex Web Bridge Connector RS",
                        "version": SERVER_VERSION
                    },
                    "instructions": instructions
                }),
            )
        }
        "ping" => rpc_result(req_id, json!({})),
        _ => {
            if !session_initialized(state, session_id.as_deref()) {
                return rpc_error(
                    req_id,
                    -32002,
                    "session not initialized; call initialize first",
                );
            }
            match method {
                "tools/list" => rpc_result(
                    req_id,
                    json!({"tools": tool_definitions(state.config.trust_level)}),
                ),
                "tools/call" => handle_tools_call(state, req_id, params, session_id.as_deref()),
                "resources/list" => rpc_result(req_id, resources_list_result()),
                "resources/read" => handle_resources_read(req_id, params),
                _ => rpc_error(req_id, -32601, &format!("unknown method: {method}")),
            }
        }
    }
}

fn handle_tools_call(
    state: &AppState,
    req_id: Option<Value>,
    params: Value,
    session_id: Option<&str>,
) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(req_id, -32602, "params.name must be a string");
    };
    let Some(arguments) = params.get("arguments").and_then(Value::as_object) else {
        return rpc_error(req_id, -32602, "params.arguments must be an object");
    };
    let result = call_tool(state, name, arguments, session_id);
    record_tool_call(state, session_id, name, arguments, &result);
    match result {
        ToolOutcome::Ok(payload) => rpc_result(req_id, tool_result(name, payload, false)),
        ToolOutcome::ToolError(message) => {
            rpc_result(req_id, tool_result(name, json!({"error": message}), true))
        }
        ToolOutcome::ProtocolError(message) => rpc_error(req_id, -32602, &message),
    }
}

fn required_tool_scope(tool: &str) -> Option<&'static str> {
    match tool {
        "list_notes"
        | "show_review"
        | "render_review"
        | "list_pull_requests"
        | "show_pull_requests"
        | "render_pull_requests"
        | "list_edit_plans"
        | "show_edit_plans"
        | "render_edit_plans" => Some("workspace:read"),
        "write"
        | "edit"
        | "apply_patch"
        | "move_path"
        | "open_worktree"
        | "publish_branch"
        | "create_pull_request"
        | "refresh_pull_request_status"
        | "refresh_pull_requests" => Some("workspace:write"),
        "shell" => Some("shell"),
        _ => None,
    }
}

fn tool_call_name(params: &Value) -> Option<&str> {
    params.get("name").and_then(Value::as_str)
}

fn resources_list_result() -> Value {
    json!({
        "resources": [
            {
                "uri": CHANGES_WIDGET_URI,
                "name": "Codex Change Summary",
                "title": "Codex Change Summary",
                "description": "Render a compact change summary for a workspace.",
                "mimeType": "text/html",
                "_meta": changes_widget_resource_meta()
            },
            {
                "uri": REVIEW_WIDGET_URI,
                "name": "Codex Review Handoff",
                "title": "Codex Review Handoff",
                "description": "Render recoverable review notes and edit plans for a workspace.",
                "mimeType": "text/html",
                "_meta": review_widget_resource_meta()
            },
            {
                "uri": PULL_REQUESTS_WIDGET_URI,
                "name": "Codex Pull Request Handoff",
                "title": "Codex Pull Request Handoff",
                "description": "Render connector-created pull request lifecycle records.",
                "mimeType": "text/html",
                "_meta": pull_requests_widget_resource_meta()
            },
            {
                "uri": EDIT_PLANS_WIDGET_URI,
                "name": "Codex Edit Plan History",
                "title": "Codex Edit Plan History",
                "description": "Render connector-created edit plan history and lifecycle status.",
                "mimeType": "text/html",
                "_meta": edit_plans_widget_resource_meta()
            }
        ]
    })
}

fn handle_resources_read(req_id: Option<Value>, params: Value) -> Value {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return rpc_error(req_id, -32602, "params.uri must be a string");
    };
    let (text, meta) = match uri {
        CHANGES_WIDGET_URI => (changes_widget_html(), changes_widget_resource_meta()),
        REVIEW_WIDGET_URI => (review_widget_html(), review_widget_resource_meta()),
        PULL_REQUESTS_WIDGET_URI => (
            pull_requests_widget_html(),
            pull_requests_widget_resource_meta(),
        ),
        EDIT_PLANS_WIDGET_URI => (edit_plans_widget_html(), edit_plans_widget_resource_meta()),
        _ => return rpc_error(req_id, -32004, "resource not found"),
    };
    rpc_result(
        req_id,
        json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/html",
                "text": text,
                "_meta": meta
            }]
        }),
    )
}

fn widget_csp_meta(description: &str) -> Value {
    json!({
        "openai/widgetDescription": description,
        "openai/widgetPrefersBorder": true,
        "openai/widgetCSP": {
            "connect_domains": [],
            "resource_domains": []
        },
        "ui": {
            "csp": {
                "connectDomains": [],
                "resourceDomains": []
            }
        }
    })
}

fn changes_widget_resource_meta() -> Value {
    widget_csp_meta("A compact change summary card for Codex workspace diffs and recent actions.")
}

fn review_widget_resource_meta() -> Value {
    widget_csp_meta("A compact review handoff card for Codex review notes and edit plans.")
}

fn pull_requests_widget_resource_meta() -> Value {
    widget_csp_meta("A compact pull request handoff card for connector-created PR records.")
}

fn edit_plans_widget_resource_meta() -> Value {
    widget_csp_meta("A compact edit plan history card for connector-created plan records.")
}

fn changes_widget_html() -> &'static str {
    r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    :root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    body { margin: 0; padding: 14px; background: Canvas; color: CanvasText; font-size: 13px; }
    .header { display: flex; justify-content: space-between; gap: 12px; align-items: baseline; margin-bottom: 10px; }
    h1 { font-size: 15px; margin: 0; font-weight: 650; }
    .muted { color: color-mix(in srgb, CanvasText 62%, transparent); }
    .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; margin-bottom: 10px; }
    .metric { border: 1px solid color-mix(in srgb, CanvasText 14%, transparent); border-radius: 8px; padding: 8px; min-width: 0; }
    .label { color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 11px; margin-bottom: 4px; }
    .value { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
    pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 220px; overflow: auto; border: 1px solid color-mix(in srgb, CanvasText 14%, transparent); border-radius: 8px; padding: 8px; margin: 0 0 10px; background: color-mix(in srgb, CanvasText 4%, transparent); }
    ul { margin: 0; padding-left: 18px; }
    li { margin: 4px 0; }
  </style>
</head>
<body>
  <div class="header">
    <h1>Workspace Changes</h1>
    <span class="muted" id="workspace"></span>
  </div>
  <div class="grid">
    <div class="metric"><div class="label">Branch</div><div class="value" id="branch">-</div></div>
    <div class="metric"><div class="label">HEAD</div><div class="value" id="head">-</div></div>
    <div class="metric"><div class="label">Truncated</div><div class="value" id="truncated">-</div></div>
  </div>
  <pre id="status">No status.</pre>
  <pre id="stat">No diff stat.</pre>
  <ul id="actions"></ul>
  <script>
    const data = window.openai?.toolOutput || window.openai?.structuredContent || {};
    const text = (value, fallback = "-") => value === undefined || value === null || value === "" ? fallback : String(value);
    document.getElementById("workspace").textContent = text(data.workspace_id, "");
    document.getElementById("branch").textContent = text(data.branch);
    document.getElementById("head").textContent = text(data.head);
    document.getElementById("truncated").textContent = data.truncated ? "yes" : "no";
    document.getElementById("status").textContent = text(data.status, "Clean working tree.");
    document.getElementById("stat").textContent = text(data.stat, "No diff stat.");
    const actions = Array.isArray(data.recent_actions) ? data.recent_actions : [];
    const list = document.getElementById("actions");
    if (!actions.length) {
      const item = document.createElement("li");
      item.textContent = "No recent change actions in this session.";
      list.appendChild(item);
    } else {
      for (const action of actions.slice(-8)) {
        const item = document.createElement("li");
        const target = action.from_path && action.to_path
          ? `${action.from_path} -> ${action.to_path}`
          : action.path || action.cwd || action.workspace_id;
        item.textContent = [action.tool, target].filter(Boolean).join(" - ");
        list.appendChild(item);
      }
    }
  </script>
</body>
</html>"#
}

fn review_widget_html() -> &'static str {
    r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    :root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    body { margin: 0; padding: 14px; background: Canvas; color: CanvasText; font-size: 13px; }
    .header { display: flex; justify-content: space-between; gap: 12px; align-items: baseline; margin-bottom: 10px; }
    h1 { font-size: 15px; margin: 0; font-weight: 650; }
    h2 { font-size: 13px; margin: 14px 0 8px; font-weight: 650; }
    .muted { color: color-mix(in srgb, CanvasText 62%, transparent); }
    .grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; margin-bottom: 10px; }
    .metric, .item { border: 1px solid color-mix(in srgb, CanvasText 14%, transparent); border-radius: 8px; padding: 8px; min-width: 0; }
    .label { color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 11px; margin-bottom: 4px; }
    .value { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
    .item { margin-bottom: 8px; }
    .item-title { font-weight: 650; margin-bottom: 4px; }
    .meta { color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 11px; margin-bottom: 6px; overflow-wrap: anywhere; }
    .body { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 180px; overflow: auto; }
    .empty { color: color-mix(in srgb, CanvasText 58%, transparent); border: 1px dashed color-mix(in srgb, CanvasText 18%, transparent); border-radius: 8px; padding: 8px; }
  </style>
</head>
<body>
  <div class="header">
    <h1>Review Handoff</h1>
    <span class="muted" id="workspace"></span>
  </div>
  <div class="grid">
    <div class="metric"><div class="label">Notes</div><div class="value" id="note-count">0</div></div>
    <div class="metric"><div class="label">Plans</div><div class="value" id="plan-count">0</div></div>
    <div class="metric"><div class="label">Truncated</div><div class="value" id="truncated">no</div></div>
  </div>
  <h2>Notes</h2>
  <div id="notes"></div>
  <h2>Edit Plans</h2>
  <div id="plans"></div>
  <script>
    const data = window.openai?.toolOutput || window.openai?.structuredContent || {};
    const text = (value, fallback = "-") => value === undefined || value === null || value === "" ? fallback : String(value);
    const notes = Array.isArray(data.notes) ? data.notes : [];
    const plans = Array.isArray(data.edit_plans) ? data.edit_plans : [];
    document.getElementById("workspace").textContent = text(data.workspace_id, "");
    document.getElementById("note-count").textContent = String(notes.length);
    document.getElementById("plan-count").textContent = String(plans.length);
    document.getElementById("truncated").textContent = data.truncated ? "yes" : "no";
    function appendEmpty(parent, label) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = label;
      parent.appendChild(empty);
    }
    function appendItem(parent, title, meta, body) {
      const item = document.createElement("div");
      item.className = "item";
      const heading = document.createElement("div");
      heading.className = "item-title";
      heading.textContent = title;
      const metaEl = document.createElement("div");
      metaEl.className = "meta";
      metaEl.textContent = meta;
      const bodyEl = document.createElement("div");
      bodyEl.className = "body";
      bodyEl.textContent = body;
      item.append(heading, metaEl, bodyEl);
      parent.appendChild(item);
    }
    const notesEl = document.getElementById("notes");
    if (!notes.length) appendEmpty(notesEl, "No review notes.");
    for (const note of notes.slice(0, 8)) {
      appendItem(notesEl, text(note.title, "Untitled note"), [note.severity, note.path].filter(Boolean).join(" · "), text(note.body, ""));
    }
    const plansEl = document.getElementById("plans");
    if (!plans.length) appendEmpty(plansEl, "No edit plans.");
    for (const plan of plans.slice(0, 8)) {
      const files = Array.isArray(plan.files) ? `${plan.files.length} files` : "0 files";
      appendItem(plansEl, text(plan.title, "Untitled plan"), [plan.status, files].filter(Boolean).join(" · "), text(plan.intent, ""));
    }
  </script>
</body>
</html>"#
}

fn pull_requests_widget_html() -> &'static str {
    r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    :root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    body { margin: 0; padding: 14px; background: Canvas; color: CanvasText; font-size: 13px; }
    .header { display: flex; justify-content: space-between; gap: 12px; align-items: baseline; margin-bottom: 10px; }
    h1 { font-size: 15px; margin: 0; font-weight: 650; }
    .muted { color: color-mix(in srgb, CanvasText 62%, transparent); }
    .grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; margin-bottom: 10px; }
    .metric, .item { border: 1px solid color-mix(in srgb, CanvasText 14%, transparent); border-radius: 8px; padding: 8px; min-width: 0; }
    .label { color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 11px; margin-bottom: 4px; }
    .value { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
    .item { margin-bottom: 8px; }
    .item-title { font-weight: 650; margin-bottom: 4px; overflow-wrap: anywhere; }
    .meta { color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 11px; overflow-wrap: anywhere; }
    .empty { color: color-mix(in srgb, CanvasText 58%, transparent); border: 1px dashed color-mix(in srgb, CanvasText 18%, transparent); border-radius: 8px; padding: 8px; }
  </style>
</head>
<body>
  <div class="header">
    <h1>Pull Request Handoff</h1>
    <span class="muted" id="scope"></span>
  </div>
  <div class="grid">
    <div class="metric"><div class="label">Records</div><div class="value" id="count">0</div></div>
    <div class="metric"><div class="label">Open</div><div class="value" id="open">0</div></div>
    <div class="metric"><div class="label">Merged</div><div class="value" id="merged">0</div></div>
    <div class="metric"><div class="label">Truncated</div><div class="value" id="truncated">no</div></div>
  </div>
  <div id="items"></div>
  <script>
    const data = window.openai?.toolOutput || window.openai?.structuredContent || {};
    const text = (value, fallback = "-") => value === undefined || value === null || value === "" ? fallback : String(value);
    const prs = Array.isArray(data.pull_requests) ? data.pull_requests : [];
    const counts = data.status_counts || {};
    document.getElementById("scope").textContent = [data.workspace_id, data.branch].filter(Boolean).join(" - ");
    document.getElementById("count").textContent = String(prs.length);
    document.getElementById("open").textContent = String(counts.open || 0);
    document.getElementById("merged").textContent = String(counts.merged || 0);
    document.getElementById("truncated").textContent = data.truncated ? "yes" : "no";
    const root = document.getElementById("items");
    if (!prs.length) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = "No pull request handoff records.";
      root.appendChild(empty);
    }
    for (const pr of prs.slice(0, 10)) {
      const item = document.createElement("div");
      item.className = "item";
      const title = document.createElement("div");
      title.className = "item-title";
      title.textContent = text(pr.title, "Untitled pull request");
      const meta = document.createElement("div");
      meta.className = "meta";
      const status = pr.remote_state || pr.status;
      const number = pr.number ? `#${pr.number}` : "";
      meta.textContent = [status, pr.branch, number, pr.url].filter(Boolean).join(" - ");
      item.append(title, meta);
      root.appendChild(item);
    }
  </script>
</body>
</html>"#
}

fn edit_plans_widget_html() -> &'static str {
    r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    :root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    body { margin: 0; padding: 14px; background: Canvas; color: CanvasText; font-size: 13px; }
    .header { display: flex; justify-content: space-between; gap: 12px; align-items: baseline; margin-bottom: 10px; }
    h1 { font-size: 15px; margin: 0; font-weight: 650; }
    .muted { color: color-mix(in srgb, CanvasText 62%, transparent); }
    .grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; margin-bottom: 10px; }
    .metric, .item { border: 1px solid color-mix(in srgb, CanvasText 14%, transparent); border-radius: 8px; padding: 8px; min-width: 0; }
    .label { color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 11px; margin-bottom: 4px; }
    .value { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
    .item { margin-bottom: 8px; }
    .item-title { font-weight: 650; margin-bottom: 4px; overflow-wrap: anywhere; }
    .meta { color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 11px; margin-bottom: 6px; overflow-wrap: anywhere; }
    .body { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 160px; overflow: auto; }
    .empty { color: color-mix(in srgb, CanvasText 58%, transparent); border: 1px dashed color-mix(in srgb, CanvasText 18%, transparent); border-radius: 8px; padding: 8px; }
  </style>
</head>
<body>
  <div class="header">
    <h1>Edit Plan History</h1>
    <span class="muted" id="scope"></span>
  </div>
  <div class="grid">
    <div class="metric"><div class="label">Plans</div><div class="value" id="count">0</div></div>
    <div class="metric"><div class="label">Draft</div><div class="value" id="draft">0</div></div>
    <div class="metric"><div class="label">Approved</div><div class="value" id="approved">0</div></div>
    <div class="metric"><div class="label">Applied</div><div class="value" id="applied">0</div></div>
  </div>
  <div id="items"></div>
  <script>
    const data = window.openai?.toolOutput || window.openai?.structuredContent || {};
    const text = (value, fallback = "-") => value === undefined || value === null || value === "" ? fallback : String(value);
    const plans = Array.isArray(data.edit_plans) ? data.edit_plans : [];
    const counts = data.status_counts || {};
    document.getElementById("scope").textContent = [data.workspace_id, data.status].filter(Boolean).join(" - ");
    document.getElementById("count").textContent = String(plans.length);
    document.getElementById("draft").textContent = String(counts.draft || 0);
    document.getElementById("approved").textContent = String(counts.approved || 0);
    document.getElementById("applied").textContent = String(counts.applied || 0);
    const root = document.getElementById("items");
    if (!plans.length) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = "No edit plans.";
      root.appendChild(empty);
    }
    for (const plan of plans.slice(0, 10)) {
      const item = document.createElement("div");
      item.className = "item";
      const title = document.createElement("div");
      title.className = "item-title";
      title.textContent = text(plan.title, "Untitled plan");
      const meta = document.createElement("div");
      meta.className = "meta";
      const pathCount = Array.isArray(plan.paths) ? `${plan.paths.length} paths` : "0 paths";
      const fileCount = Array.isArray(plan.files) ? `${plan.files.length} patch files` : "0 patch files";
      meta.textContent = [plan.status, pathCount, fileCount, plan.plan_id].filter(Boolean).join(" - ");
      const body = document.createElement("div");
      body.className = "body";
      body.textContent = text(plan.intent, "");
      item.append(title, meta, body);
      root.appendChild(item);
    }
  </script>
</body>
</html>"#
}

fn record_tool_call(
    state: &AppState,
    session_id: Option<&str>,
    name: &str,
    arguments: &serde_json::Map<String, Value>,
    result: &ToolOutcome,
) {
    let Some(state_dir) = &state.config.state_dir else {
        return;
    };
    let (outcome, result_summary, error) = match result {
        ToolOutcome::Ok(payload) => ("ok", summarize_result(name, payload), None),
        ToolOutcome::ToolError(message) => ("tool_error", json!({}), Some(message.clone())),
        ToolOutcome::ProtocolError(message) => ("protocol_error", json!({}), Some(message.clone())),
    };
    let entry = json!({
        "ts_unix_ms": unix_ms(),
        "event": "tool_call",
        "session_id": session_id,
        "tool": name,
        "arguments": sanitize_arguments(arguments),
        "outcome": outcome,
        "result": result_summary,
        "error": error
    });
    let _ = append_audit_event(state_dir, &entry);
    record_persisted_tool_call(state, session_id, name, arguments, result);
    if let ToolOutcome::Ok(payload) = result {
        record_workspace_from_tool_result(state, session_id, name, arguments, payload);
        record_pull_request_from_tool_result(state, session_id, name, arguments, payload);
    }
}

fn record_workspace_from_tool_result(
    state: &AppState,
    session_id: Option<&str>,
    name: &str,
    arguments: &serde_json::Map<String, Value>,
    payload: &Value,
) {
    match name {
        "open_workspace" => {
            let Some(workspace_id) = payload.get("workspace_id").and_then(Value::as_str) else {
                return;
            };
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("workspace");
            record_workspace_opened(
                state,
                session_id,
                WorkspaceOpenRecord {
                    workspace_id,
                    name,
                    kind: "workspace",
                    source_workspace_id: None,
                    branch: None,
                    base_ref: None,
                    task_id: None,
                    task: None,
                },
            );
        }
        "open_worktree" => {
            let Some(workspace_id) = payload.get("workspace_id").and_then(Value::as_str) else {
                return;
            };
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("workspace");
            record_workspace_opened(
                state,
                session_id,
                WorkspaceOpenRecord {
                    workspace_id,
                    name,
                    kind: "worktree",
                    source_workspace_id: arguments.get("workspace_id").and_then(Value::as_str),
                    branch: payload.get("branch").and_then(Value::as_str),
                    base_ref: payload.get("base_ref").and_then(Value::as_str),
                    task_id: payload.get("task_id").and_then(Value::as_str),
                    task: payload.get("task").and_then(Value::as_str),
                },
            );
        }
        _ => {}
    }
}

fn record_pull_request_from_tool_result(
    state: &AppState,
    session_id: Option<&str>,
    name: &str,
    arguments: &serde_json::Map<String, Value>,
    payload: &Value,
) {
    if name != "create_pull_request" {
        return;
    }
    let Some(workspace_id) = arguments.get("workspace_id").and_then(Value::as_str) else {
        return;
    };
    let Some(branch) = payload.get("branch").and_then(Value::as_str) else {
        return;
    };
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Pull request");
    let success = payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let now = unix_ms();
    let record = PersistedPullRequest {
        created_unix_ms: now,
        updated_unix_ms: now,
        session_id: session_id.map(ToString::to_string),
        workspace_id: truncate_string(workspace_id, 120),
        branch: truncate_string(branch, 200),
        base: payload
            .get("base")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 200)),
        title: truncate_string(title, 200),
        draft: payload
            .get("draft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status: if success { "created" } else { "failed" }.to_string(),
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 500)),
        number: None,
        remote_state: None,
        merged: None,
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        body_chars: payload
            .get("body_chars")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
    };
    update_persisted_state(state, |snapshot| {
        snapshot.pull_requests.push(record);
    });
}

fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn append_audit_event(state_dir: &Path, entry: &Value) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join("audit.jsonl");
    if fs::metadata(&path)
        .map(|meta| meta.len() > MAX_AUDIT_LOG_BYTES)
        .unwrap_or(false)
    {
        let rotated = state_dir.join("audit.jsonl.1");
        let _ = fs::remove_file(&rotated);
        fs::rename(&path, rotated)?;
    }
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn append_review_note(state_dir: &Path, entry: &Value) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join("review-notes.jsonl");
    if fs::metadata(&path)
        .map(|meta| meta.len() > MAX_REVIEW_NOTES_LOG_BYTES)
        .unwrap_or(false)
    {
        let rotated = state_dir.join("review-notes.jsonl.1");
        let _ = fs::remove_file(&rotated);
        fs::rename(&path, rotated)?;
    }
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn read_review_notes(
    state_dir: &Path,
    workspace_id: Option<&str>,
    severity: Option<&str>,
    path_filter: Option<&str>,
    limit: usize,
) -> Result<(Vec<Value>, bool)> {
    let path = state_dir.join("review-notes.jsonl");
    if !path.exists() {
        return Ok((vec![], false));
    }
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let limit = limit.min(MAX_REVIEW_NOTES).min(MAX_AUDIT_EVENTS_LIMIT);
    let mut notes: VecDeque<Value> = VecDeque::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_REVIEW_NOTE_LINE_BYTES {
            continue;
        }
        let Ok(note) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if workspace_id
            .map(|id| note.get("workspace_id").and_then(Value::as_str) == Some(id))
            .unwrap_or(true)
            && severity
                .map(|severity| note.get("severity").and_then(Value::as_str) == Some(severity))
                .unwrap_or(true)
            && path_filter
                .map(|path| note.get("path").and_then(Value::as_str) == Some(path))
                .unwrap_or(true)
        {
            notes.push_back(note);
            while notes.len() > limit.saturating_add(1) {
                notes.pop_front();
            }
        }
    }
    let mut notes: Vec<Value> = notes.into_iter().rev().collect();
    let truncated = notes.len() > limit;
    notes.truncate(limit);
    Ok((notes, truncated))
}

fn read_audit_events(state_dir: &Path, limit: usize) -> Result<Vec<Value>> {
    let limit = limit.min(MAX_AUDIT_EVENTS_LIMIT);
    let path = state_dir.join("audit.jsonl");
    if !path.exists() {
        return Ok(vec![]);
    }
    let file = fs::File::open(path)?;
    let mut tail: VecDeque<Value> = VecDeque::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        tail.push_back(serde_json::from_str(&line)?);
        while tail.len() > limit {
            tail.pop_front();
        }
    }
    Ok(tail.into_iter().collect())
}

fn read_audit_events_for_session(
    state_dir: &Path,
    session_id: &str,
    limit: usize,
) -> Result<Vec<Value>> {
    let limit = limit.min(MAX_AUDIT_EVENTS_LIMIT);
    let path = state_dir.join("audit.jsonl");
    if !path.exists() {
        return Ok(vec![]);
    }
    let file = fs::File::open(path)?;
    let mut tail: VecDeque<Value> = VecDeque::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(&line)?;
        if event.get("session_id").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        tail.push_back(event);
        while tail.len() > limit {
            tail.pop_front();
        }
    }
    Ok(tail.into_iter().collect())
}

fn summarize_sessions(events: &[Value]) -> Vec<Value> {
    let mut summaries: HashMap<String, (u64, u64, HashSet<String>)> = HashMap::new();
    for event in events {
        let Some(session_id) = event.get("session_id").and_then(Value::as_str) else {
            continue;
        };
        let ts = event
            .get("ts_unix_ms")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let tool = event.get("tool").and_then(Value::as_str).unwrap_or("-");
        let entry = summaries
            .entry(session_id.to_string())
            .or_insert((0, 0, HashSet::new()));
        entry.0 += 1;
        entry.1 = entry.1.max(ts);
        entry.2.insert(tool.to_string());
    }
    let mut out: Vec<Value> = summaries
        .into_iter()
        .map(|(session_id, (call_count, last_ts, tools))| {
            let mut tools: Vec<String> = tools.into_iter().collect();
            tools.sort();
            json!({
                "session_id": session_id,
                "call_count": call_count,
                "last_ts_unix_ms": last_ts,
                "tools": tools
            })
        })
        .collect();
    out.sort_by_key(|item| item["last_ts_unix_ms"].as_u64().unwrap_or_default());
    out.reverse();
    out
}

fn session_detail(events: &[Value], session_id: &str) -> Value {
    let calls: Vec<Value> = events
        .iter()
        .filter(|event| event.get("session_id").and_then(Value::as_str) == Some(session_id))
        .cloned()
        .collect();
    json!({
        "session_id": session_id,
        "call_count": calls.len(),
        "calls": calls,
    })
}

fn record_session_initialized(state: &AppState, session_id: &str) {
    update_persisted_state(state, |snapshot| {
        let session = persisted_session_mut(snapshot, session_id);
        session.initialized = true;
        session.last_unix_ms = unix_ms();
    });
}

fn record_workspace_opened(
    state: &AppState,
    session_id: Option<&str>,
    record: WorkspaceOpenRecord<'_>,
) {
    let Some(session_id) = session_id else {
        return;
    };
    update_persisted_state(state, |snapshot| {
        let session = persisted_session_mut(snapshot, session_id);
        session.last_unix_ms = unix_ms();
        session
            .workspaces
            .retain(|workspace| workspace.workspace_id != record.workspace_id);
        session.workspaces.push(PersistedWorkspace {
            workspace_id: record.workspace_id.to_string(),
            name: truncate_string(record.name, 120),
            kind: record.kind.to_string(),
            opened_unix_ms: unix_ms(),
            source_workspace_id: record.source_workspace_id.map(ToString::to_string),
            branch: record.branch.map(|value| truncate_string(value, 200)),
            base_ref: record.base_ref.map(|value| truncate_string(value, 200)),
            task_id: record.task_id.map(|value| truncate_string(value, 120)),
            task: record.task.map(|value| truncate_string(value, 500)),
        });
        while session.workspaces.len() > MAX_SESSION_WORKSPACES {
            session.workspaces.remove(0);
        }
    });
}

fn record_persisted_tool_call(
    state: &AppState,
    session_id: Option<&str>,
    name: &str,
    arguments: &serde_json::Map<String, Value>,
    result: &ToolOutcome,
) {
    let Some(session_id) = session_id else {
        return;
    };
    let (outcome, error) = match result {
        ToolOutcome::Ok(_) => ("ok", None),
        ToolOutcome::ToolError(message) => ("tool_error", Some(truncate_string(message, 240))),
        ToolOutcome::ProtocolError(message) => {
            ("protocol_error", Some(truncate_string(message, 240)))
        }
    };
    let call = PersistedToolCall {
        ts_unix_ms: unix_ms(),
        tool: name.to_string(),
        outcome: outcome.to_string(),
        workspace_id: arguments
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 120)),
        path: arguments
            .get("path")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 240)),
        from_path: arguments
            .get("from_path")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 240)),
        to_path: arguments
            .get("to_path")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 240)),
        plan_id: arguments
            .get("plan_id")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 120)),
        query: arguments
            .get("query")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 240)),
        cwd: arguments
            .get("cwd")
            .and_then(Value::as_str)
            .map(|value| truncate_string(value, 240)),
        error,
    };
    update_persisted_state(state, |snapshot| {
        let session = persisted_session_mut(snapshot, session_id);
        session.last_unix_ms = call.ts_unix_ms;
        session.tool_calls.push(call);
        while session.tool_calls.len() > MAX_SESSION_TOOL_CALLS {
            session.tool_calls.remove(0);
        }
    });
}

fn update_persisted_state<F>(state: &AppState, update: F)
where
    F: FnOnce(&mut PersistedState),
{
    let Some(persisted) = &state.persisted_state else {
        return;
    };
    let Some(state_dir) = &state.config.state_dir else {
        return;
    };
    let snapshot = {
        let mut guard = persisted.lock().unwrap();
        update(&mut guard);
        cap_persisted_state(&mut guard);
        guard.clone()
    };
    let _ = persist_state_snapshot(state_dir, &snapshot);
}

fn persisted_session_mut<'a>(
    snapshot: &'a mut PersistedState,
    session_id: &str,
) -> &'a mut PersistedSession {
    if let Some(idx) = snapshot
        .sessions
        .iter()
        .position(|session| session.session_id == session_id)
    {
        return &mut snapshot.sessions[idx];
    }
    let now = unix_ms();
    snapshot.sessions.push(PersistedSession {
        session_id: session_id.to_string(),
        created_unix_ms: now,
        last_unix_ms: now,
        initialized: false,
        workspaces: vec![],
        tool_calls: vec![],
    });
    snapshot.sessions.last_mut().unwrap()
}

fn cap_persisted_state(snapshot: &mut PersistedState) {
    snapshot
        .sessions
        .sort_by_key(|session| session.last_unix_ms);
    while snapshot.sessions.len() > MAX_PERSISTED_SESSIONS {
        snapshot.sessions.remove(0);
    }
    for session in &mut snapshot.sessions {
        while session.tool_calls.len() > MAX_SESSION_TOOL_CALLS {
            session.tool_calls.remove(0);
        }
        while session.workspaces.len() > MAX_SESSION_WORKSPACES {
            session.workspaces.remove(0);
        }
    }
    snapshot
        .pull_requests
        .sort_by_key(|pull_request| pull_request.created_unix_ms);
    while snapshot.pull_requests.len() > MAX_PERSISTED_PULL_REQUESTS {
        snapshot.pull_requests.remove(0);
    }
    snapshot
        .edit_plans
        .sort_by_key(|edit_plan| edit_plan.created_unix_ms);
    while snapshot.edit_plans.len() > MAX_PERSISTED_EDIT_PLANS {
        snapshot.edit_plans.remove(0);
    }
}

fn persisted_session_detail(state_dir: &Path, session_id: &str) -> Result<Option<Value>> {
    let snapshot = load_persisted_state(state_dir)?;
    Ok(snapshot
        .sessions
        .into_iter()
        .find(|session| session.session_id == session_id)
        .map(persisted_session_to_value))
}

fn persisted_session_summaries(state_dir: &Path, limit: usize) -> Result<Vec<Value>> {
    let mut sessions = load_persisted_state(state_dir)?.sessions;
    sessions.sort_by_key(|session| session.last_unix_ms);
    sessions.reverse();
    Ok(sessions
        .into_iter()
        .take(limit.min(MAX_AUDIT_EVENTS_LIMIT))
        .map(|session| {
            let mut tools: Vec<String> = session
                .tool_calls
                .iter()
                .map(|call| call.tool.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            tools.sort();
            json!({
                "session_id": session.session_id,
                "call_count": session.tool_calls.len(),
                "workspace_count": session.workspaces.len(),
                "last_ts_unix_ms": session.last_unix_ms,
                "tools": tools
            })
        })
        .collect())
}

fn persisted_session_to_value(session: PersistedSession) -> Value {
    json!({
        "session_id": session.session_id,
        "created_unix_ms": session.created_unix_ms,
        "last_unix_ms": session.last_unix_ms,
        "initialized": session.initialized,
        "workspace_count": session.workspaces.len(),
        "call_count": session.tool_calls.len(),
        "workspaces": session.workspaces,
        "calls": session.tool_calls,
    })
}

fn persisted_pull_requests(
    state_dir: &Path,
    workspace_id: Option<&str>,
    branch: Option<&str>,
    limit: usize,
) -> Result<(Vec<PersistedPullRequest>, bool)> {
    let mut pull_requests = load_persisted_state(state_dir)?.pull_requests;
    pull_requests.retain(|pull_request| {
        workspace_id
            .map(|id| pull_request.workspace_id == id)
            .unwrap_or(true)
            && branch
                .map(|branch| pull_request.branch == branch)
                .unwrap_or(true)
    });
    pull_requests.sort_by_key(|pull_request| pull_request.created_unix_ms);
    pull_requests.reverse();
    let limit = limit.min(MAX_AUDIT_EVENTS_LIMIT);
    let truncated = pull_requests.len() > limit;
    pull_requests.truncate(limit);
    Ok((pull_requests, truncated))
}

fn update_persisted_pull_request<F>(
    state: &AppState,
    workspace_id: &str,
    branch: Option<&str>,
    url: Option<&str>,
    update: F,
) -> Result<PersistedPullRequest>
where
    F: FnOnce(&mut PersistedPullRequest) -> Result<()>,
{
    let Some(persisted) = &state.persisted_state else {
        bail!("persisted state is not available for pull requests");
    };
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for pull request state");
    };
    let (snapshot, pull_request) = {
        let mut guard = persisted.lock().unwrap();
        let Some(idx) =
            guard
                .pull_requests
                .iter()
                .enumerate()
                .rev()
                .find_map(|(idx, pull_request)| {
                    let workspace_matches = pull_request.workspace_id == workspace_id;
                    let branch_matches = branch
                        .map(|branch| pull_request.branch == branch)
                        .unwrap_or(true);
                    let url_matches = url
                        .map(|url| pull_request.url.as_deref() == Some(url))
                        .unwrap_or(true);
                    (workspace_matches && branch_matches && url_matches).then_some(idx)
                })
        else {
            bail!("pull request handoff record not found");
        };
        let pull_request = &mut guard.pull_requests[idx];
        update(pull_request)?;
        pull_request.updated_unix_ms = unix_ms();
        let pull_request = pull_request.clone();
        cap_persisted_state(&mut guard);
        (guard.clone(), pull_request)
    };
    persist_state_snapshot(state_dir, &snapshot)?;
    Ok(pull_request)
}

fn update_persisted_pull_request_by_key<F>(
    state: &AppState,
    workspace_id: &str,
    key: &PullRequestRecordKey,
    update: F,
) -> Result<PersistedPullRequest>
where
    F: FnOnce(&mut PersistedPullRequest) -> Result<()>,
{
    let Some(persisted) = &state.persisted_state else {
        bail!("persisted state is not available for pull requests");
    };
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for pull request state");
    };
    let (snapshot, pull_request) = {
        let mut guard = persisted.lock().unwrap();
        let Some(idx) =
            guard
                .pull_requests
                .iter()
                .enumerate()
                .rev()
                .find_map(|(idx, pull_request)| {
                    let workspace_matches = pull_request.workspace_id == workspace_id;
                    let created_matches = pull_request.created_unix_ms == key.created_unix_ms;
                    let branch_matches = pull_request.branch == key.branch;
                    let url_matches = pull_request.url == key.url;
                    (workspace_matches && created_matches && branch_matches && url_matches)
                        .then_some(idx)
                })
        else {
            bail!("pull request handoff record not found");
        };
        let pull_request = &mut guard.pull_requests[idx];
        update(pull_request)?;
        pull_request.updated_unix_ms = unix_ms();
        let pull_request = pull_request.clone();
        cap_persisted_state(&mut guard);
        (guard.clone(), pull_request)
    };
    persist_state_snapshot(state_dir, &snapshot)?;
    Ok(pull_request)
}

fn update_persisted_pull_request_for_refresh<F>(
    state: &AppState,
    workspace_id: &str,
    record_key: Option<&PullRequestRecordKey>,
    branch: Option<&str>,
    url: Option<&str>,
    update: F,
) -> Result<PersistedPullRequest>
where
    F: FnOnce(&mut PersistedPullRequest) -> Result<()>,
{
    if let Some(record_key) = record_key {
        update_persisted_pull_request_by_key(state, workspace_id, record_key, update)
    } else {
        update_persisted_pull_request(state, workspace_id, branch, url, update)
    }
}

fn persisted_edit_plans(
    state_dir: &Path,
    workspace_id: Option<&str>,
    status: Option<&str>,
    limit: usize,
) -> Result<(Vec<PersistedEditPlan>, bool)> {
    let mut edit_plans = load_persisted_state(state_dir)?.edit_plans;
    edit_plans.retain(|edit_plan| {
        workspace_id
            .map(|id| edit_plan.workspace_id == id)
            .unwrap_or(true)
            && status
                .map(|status| edit_plan.status == status)
                .unwrap_or(true)
    });
    edit_plans.sort_by_key(|edit_plan| edit_plan.created_unix_ms);
    edit_plans.reverse();
    let limit = limit.min(MAX_AUDIT_EVENTS_LIMIT);
    let truncated = edit_plans.len() > limit;
    edit_plans.truncate(limit);
    Ok((edit_plans, truncated))
}

fn update_persisted_edit_plan<F>(
    state: &AppState,
    plan_id: &str,
    update: F,
) -> Result<PersistedEditPlan>
where
    F: FnOnce(&mut PersistedEditPlan) -> Result<()>,
{
    let Some(persisted) = &state.persisted_state else {
        bail!("persisted state is not available for edit plans");
    };
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for edit plan state");
    };
    let (snapshot, plan) = {
        let mut guard = persisted.lock().unwrap();
        let Some(plan) = guard
            .edit_plans
            .iter_mut()
            .find(|edit_plan| edit_plan.plan_id == plan_id)
        else {
            bail!("edit plan not found: {plan_id}");
        };
        update(plan)?;
        plan.updated_unix_ms = unix_ms();
        let plan = plan.clone();
        cap_persisted_state(&mut guard);
        (guard.clone(), plan)
    };
    persist_state_snapshot(state_dir, &snapshot)?;
    Ok(plan)
}

fn recent_change_actions(
    state: &AppState,
    session_id: &str,
    workspace_id: &str,
) -> Result<Vec<Value>> {
    let Some(state_dir) = &state.config.state_dir else {
        return Ok(vec![]);
    };
    let Some(detail) = persisted_session_detail(state_dir, session_id)? else {
        return Ok(vec![]);
    };
    let Some(calls) = detail.get("calls").and_then(Value::as_array) else {
        return Ok(vec![]);
    };
    let mut actions: Vec<Value> = calls
        .iter()
        .rev()
        .filter(|call| {
            matches!(
                call.get("tool").and_then(Value::as_str),
                Some(
                    "write"
                        | "edit"
                        | "apply_patch"
                        | "move_path"
                        | "shell"
                        | "open_worktree"
                        | "publish_branch"
                        | "create_pull_request"
                        | "refresh_pull_request_status"
                        | "refresh_pull_requests"
                )
            ) && call.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
        })
        .take(MAX_RECENT_CHANGE_ACTIONS)
        .cloned()
        .collect();
    actions.reverse();
    Ok(actions)
}

fn sanitize_arguments(args: &serde_json::Map<String, Value>) -> Value {
    let mut out = serde_json::Map::new();
    for (key, value) in args {
        if matches!(
            key.as_str(),
            "content"
                | "contents"
                | "text"
                | "new_text"
                | "old_text"
                | "replacement"
                | "patch"
                | "intent"
                | "status_note"
                | "stdin"
                | "env"
                | "command"
                | "body"
        ) {
            out.insert(key.clone(), json!("<redacted>"));
        } else {
            out.insert(key.clone(), sanitize_value(value));
        }
    }
    Value::Object(out)
}

fn sanitize_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.len() > 240 => {
            let prefix: String = text.chars().take(240).collect();
            Value::String(format!("{prefix}...<truncated>"))
        }
        Value::Array(items) => Value::Array(items.iter().take(20).map(sanitize_value).collect()),
        Value::Object(map) => sanitize_arguments(map),
        _ => value.clone(),
    }
}

fn summarize_result(tool: &str, payload: &Value) -> Value {
    match tool {
        "open_workspace" => json!({
            "workspace_id": payload.get("workspace_id"),
            "name": payload.get("name"),
            "instructions": payload.get("instructions").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "available_instructions": payload.get("available_instructions").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "skills": payload.get("skills").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
        }),
        "read" => json!({
            "path": payload.get("path"),
            "content_chars": payload.get("content").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "list" => json!({
            "entries": payload.get("entries").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "search" => json!({
            "results": payload.get("results").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "git_status" => json!({
            "branch": payload.get("branch"),
            "head": payload.get("head"),
            "status_chars": payload.get("status").and_then(Value::as_str).map(str::len).unwrap_or(0)
        }),
        "git_diff" | "show_changes" => json!({
            "stat_chars": payload.get("stat").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "diff_chars": payload.get("diff").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated"),
            "recent_actions": payload.get("recent_actions").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
        }),
        "render_changes" => json!({
            "stat_chars": payload.get("stat").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "diff_chars": payload.get("diff").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated"),
            "recent_actions": payload.get("recent_actions").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "resource_uri": CHANGES_WIDGET_URI
        }),
        "show_review" | "render_review" => json!({
            "workspace_id": payload.get("workspace_id"),
            "notes": payload.get("notes").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "edit_plans": payload.get("edit_plans").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated"),
            "resource_uri": if tool == "render_review" { Value::String(REVIEW_WIDGET_URI.to_string()) } else { Value::Null }
        }),
        "show_edit_plans" | "render_edit_plans" => json!({
            "workspace_id": payload.get("workspace_id"),
            "status": payload.get("status"),
            "edit_plans": payload.get("edit_plans").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "status_counts": payload.get("status_counts"),
            "truncated": payload.get("truncated"),
            "resource_uri": if tool == "render_edit_plans" { Value::String(EDIT_PLANS_WIDGET_URI.to_string()) } else { Value::Null }
        }),
        "show_pull_requests" | "render_pull_requests" => json!({
            "workspace_id": payload.get("workspace_id"),
            "branch": payload.get("branch"),
            "pull_requests": payload.get("pull_requests").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "status_counts": payload.get("status_counts"),
            "truncated": payload.get("truncated"),
            "resource_uri": if tool == "render_pull_requests" { Value::String(PULL_REQUESTS_WIDGET_URI.to_string()) } else { Value::Null }
        }),
        "write" | "edit" | "apply_patch" | "preview_patch" => json!({
            "path": payload.get("path"),
            "files": payload.get("files").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "bytes_before": payload.get("bytes_before"),
            "bytes_after": payload.get("bytes_after"),
            "plan_id": payload.get("plan_id"),
            "plan_status": payload.get("plan_status"),
            "diff_chars": payload.get("diff").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "move_path" => json!({
            "from_path": payload.get("from_path"),
            "to_path": payload.get("to_path"),
            "overwritten": payload.get("overwritten"),
            "bytes": payload.get("bytes")
        }),
        "shell" => json!({
            "cwd": payload.get("cwd"),
            "exit_code": payload.get("exit_code"),
            "timed_out": payload.get("timed_out"),
            "stdout_chars": payload.get("stdout").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "stderr_chars": payload.get("stderr").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "open_worktree" => json!({
            "workspace_id": payload.get("workspace_id"),
            "name": payload.get("name"),
            "branch": payload.get("branch"),
            "base_ref": payload.get("base_ref"),
            "task_id": payload.get("task_id"),
            "task_chars": payload.get("task").and_then(Value::as_str).map(str::len).unwrap_or(0)
        }),
        "create_note" => json!({
            "note_id": payload.get("note_id"),
            "workspace_id": payload.get("workspace_id"),
            "title": payload.get("title"),
            "severity": payload.get("severity"),
            "path": payload.get("path"),
            "body_chars": payload.get("body_chars")
        }),
        "publish_branch" => json!({
            "branch": payload.get("branch"),
            "remote": payload.get("remote"),
            "remote_branch": payload.get("remote_branch"),
            "exit_code": payload.get("exit_code"),
            "success": payload.get("success"),
            "stdout_chars": payload.get("stdout").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "stderr_chars": payload.get("stderr").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "create_pull_request" => json!({
            "branch": payload.get("branch"),
            "base": payload.get("base"),
            "title": payload.get("title"),
            "draft": payload.get("draft"),
            "exit_code": payload.get("exit_code"),
            "success": payload.get("success"),
            "url": payload.get("url"),
            "body_chars": payload.get("body_chars"),
            "stdout_chars": payload.get("stdout").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "stderr_chars": payload.get("stderr").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "refresh_pull_request_status" => json!({
            "success": payload.get("success"),
            "selector": payload.get("selector"),
            "exit_code": payload.get("exit_code"),
            "status": payload.pointer("/pull_request/status"),
            "url": payload.pointer("/pull_request/url"),
            "number": payload.pointer("/pull_request/number"),
            "stdout_chars": payload.get("stdout").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "stderr_chars": payload.get("stderr").and_then(Value::as_str).map(str::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "refresh_pull_requests" => json!({
            "workspace_id": payload.get("workspace_id"),
            "branch": payload.get("branch"),
            "refreshed": payload.get("refreshed").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "succeeded": payload.get("succeeded"),
            "failed": payload.get("failed"),
            "truncated": payload.get("truncated")
        }),
        "list_worktrees" => json!({
            "worktrees": payload.get("worktrees").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "list_pull_requests" => json!({
            "pull_requests": payload.get("pull_requests").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "list_notes" => json!({
            "notes": payload.get("notes").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "create_edit_plan" => json!({
            "plan_id": payload.get("plan_id"),
            "workspace_id": payload.get("workspace_id"),
            "status": payload.get("status"),
            "title": payload.get("title"),
            "intent_chars": payload.get("intent_chars"),
            "paths": payload.get("paths").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "patch_chars": payload.get("patch_chars"),
            "files": payload.get("files").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
        }),
        "update_edit_plan_status" => json!({
            "plan_id": payload.get("plan_id"),
            "workspace_id": payload.get("workspace_id"),
            "status": payload.get("status"),
            "title": payload.get("title"),
            "status_note_chars": payload.get("status_note").and_then(Value::as_str).map(str::len).unwrap_or(0)
        }),
        "list_edit_plans" => json!({
            "edit_plans": payload.get("edit_plans").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        "list_skills" => json!({
            "skills": payload.get("skills").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "truncated": payload.get("truncated")
        }),
        _ => json!({}),
    }
}

enum ToolOutcome {
    Ok(Value),
    ToolError(String),
    ProtocolError(String),
}

fn call_tool(
    state: &AppState,
    name: &str,
    args: &serde_json::Map<String, Value>,
    session_id: Option<&str>,
) -> ToolOutcome {
    let result = match name {
        "open_workspace" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return ToolOutcome::ProtocolError("missing required argument: path".to_string());
            };
            open_workspace(state, path)
        }
        "read" => read_file_tool(state, args),
        "list" => list_tool(state, args),
        "search" => search_tool(state, args),
        "git_status" => git_status_tool(state, args),
        "git_diff" => git_diff_tool(state, args),
        "preview_patch" => preview_patch_tool(state, args),
        "show_session" => show_session_tool(state, args, session_id),
        "show_changes" => show_changes_tool(state, args, session_id),
        "render_changes" => show_changes_tool(state, args, session_id),
        "show_review" => show_review_tool(state, args),
        "render_review" => show_review_tool(state, args),
        "list_worktrees" => list_worktrees_tool(state),
        "list_pull_requests" => list_pull_requests_tool(state, args),
        "show_pull_requests" => show_pull_requests_tool(state, args),
        "render_pull_requests" => show_pull_requests_tool(state, args),
        "list_notes" => list_notes_tool(state, args),
        "list_edit_plans" => list_edit_plans_tool(state, args),
        "show_edit_plans" => show_edit_plans_tool(state, args),
        "render_edit_plans" => show_edit_plans_tool(state, args),
        "list_skills" => list_skills_tool(state),
        "create_note" => {
            if !matches!(
                state.config.trust_level,
                TrustLevel::Review | TrustLevel::Execute
            ) {
                return ToolOutcome::ToolError(
                    "create_note requires trust_level=review or trust_level=execute".to_string(),
                );
            }
            create_note_tool(state, args, session_id)
        }
        "create_edit_plan" => {
            if !matches!(
                state.config.trust_level,
                TrustLevel::Review | TrustLevel::Execute
            ) {
                return ToolOutcome::ToolError(
                    "create_edit_plan requires trust_level=review or trust_level=execute"
                        .to_string(),
                );
            }
            create_edit_plan_tool(state, args, session_id)
        }
        "update_edit_plan_status" => {
            if !matches!(
                state.config.trust_level,
                TrustLevel::Review | TrustLevel::Execute
            ) {
                return ToolOutcome::ToolError(
                    "update_edit_plan_status requires trust_level=review or trust_level=execute"
                        .to_string(),
                );
            }
            update_edit_plan_status_tool(state, args)
        }
        "write" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError("write requires trust_level=execute".to_string());
            }
            write_file_tool(state, args)
        }
        "edit" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError("edit requires trust_level=execute".to_string());
            }
            edit_file_tool(state, args)
        }
        "apply_patch" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError(
                    "apply_patch requires trust_level=execute".to_string(),
                );
            }
            apply_patch_tool(state, args, session_id)
        }
        "move_path" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError(
                    "move_path requires trust_level=execute".to_string(),
                );
            }
            move_path_tool(state, args)
        }
        "shell" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError("shell requires trust_level=execute".to_string());
            }
            shell_tool(state, args)
        }
        "open_worktree" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError(
                    "open_worktree requires trust_level=execute".to_string(),
                );
            }
            open_worktree_tool(state, args)
        }
        "publish_branch" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError(
                    "publish_branch requires trust_level=execute".to_string(),
                );
            }
            publish_branch_tool(state, args)
        }
        "create_pull_request" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError(
                    "create_pull_request requires trust_level=execute".to_string(),
                );
            }
            create_pull_request_tool(state, args)
        }
        "refresh_pull_request_status" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError(
                    "refresh_pull_request_status requires trust_level=execute".to_string(),
                );
            }
            refresh_pull_request_status_tool(state, args)
        }
        "refresh_pull_requests" => {
            if state.config.trust_level != TrustLevel::Execute {
                return ToolOutcome::ToolError(
                    "refresh_pull_requests requires trust_level=execute".to_string(),
                );
            }
            refresh_pull_requests_tool(state, args)
        }
        _ => return ToolOutcome::ProtocolError(format!("unknown tool: {name}")),
    };
    match result {
        Ok(value) => ToolOutcome::Ok(value),
        Err(err) => ToolOutcome::ToolError(err.to_string()),
    }
}

fn show_session_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    current_session_id: Option<&str>,
) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is not configured; audit logging is disabled");
    };
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .or(current_session_id)
        .ok_or_else(|| anyhow!("session_id is required when no current session is available"))?;
    if let Some(detail) = persisted_session_detail(state_dir, session_id)? {
        return Ok(detail);
    }
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
    let events = read_audit_events_for_session(state_dir, session_id, limit)?;
    Ok(session_detail(&events, session_id))
}

fn open_workspace(state: &AppState, path: &str) -> Result<Value> {
    let resolved = expand_home(Path::new(path))
        .canonicalize()
        .with_context(|| "path does not exist")?;
    if !resolved.is_dir() {
        bail!("path is not a directory");
    }
    if !state
        .config
        .allowed_roots
        .iter()
        .any(|root| is_contained(root, &resolved))
    {
        bail!("path is not inside any allowed root");
    }
    let (id, name, workspace) = register_workspace_root(state, resolved);
    let instructions = workspace_instructions(&workspace.root)?;
    let (skills, skills_truncated) = skill_summaries(&state.config, Some(&workspace.root))?;
    Ok(json!({
        "workspace_id": id,
        "name": name,
        "instructions": instructions.loaded,
        "available_instructions": instructions.available,
        "available_instructions_truncated": instructions.truncated,
        "skills": skills,
        "skills_truncated": skills_truncated
    }))
}

fn register_workspace_root(state: &AppState, root: PathBuf) -> (String, String, Workspace) {
    let id = format!("ws_{}", Uuid::new_v4().simple());
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_string();
    let workspace = Workspace {
        root,
        activated_skill_dirs: HashSet::new(),
    };
    let mut registry = state.registry.lock().unwrap();
    registry.workspaces.insert(id.clone(), workspace.clone());
    registry.order.push_back(id.clone());
    while registry.order.len() > MAX_OPEN_WORKSPACES {
        if let Some(oldest) = registry.order.pop_front() {
            registry.workspaces.remove(&oldest);
        }
    }
    (id, name, workspace)
}

fn workspace(state: &AppState, id: &str) -> Result<Workspace> {
    state
        .registry
        .lock()
        .unwrap()
        .workspaces
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("unknown workspace id: {id}"))
}

fn read_file_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let workspace_id = required(args, "workspace_id")?;
    let ws = workspace(state, workspace_id)?;
    let rel = required(args, "path")?;
    let target = resolve_read_target(state, &ws, rel)?;
    let metadata = fs::metadata(&target.absolute_path)?;
    let (content, truncated_by_read) = read_bounded_text(&target.absolute_path, MAX_READ_BYTES)?;
    if let Some(skill_dir) = target.activate_skill_dir {
        activate_skill_dir(state, workspace_id, skill_dir);
    }
    Ok(json!({
        "path": target.display_path,
        "content": content,
        "truncated": truncated_by_read || metadata.len() as usize > MAX_READ_BYTES
    }))
}

fn list_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let rel = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let target = resolve_dir(&ws, rel)?;
    let mut children = Vec::new();
    let mut truncated = false;
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if IGNORED_DIRS.contains(&name.as_str()) {
            continue;
        }
        if children.len() >= MAX_LIST_ENTRIES {
            truncated = true;
            break;
        }
        children.push(entry);
    }
    children.sort_by_key(|entry| entry.file_name());
    let mut entries = Vec::new();
    for entry in children {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = fs::symlink_metadata(entry.path())?;
        entries.push(json!({
            "name": name,
            "dir": meta.is_dir() && !meta.file_type().is_symlink(),
            "symlink": meta.file_type().is_symlink()
        }));
    }
    Ok(json!({"entries": entries, "truncated": truncated}))
}

fn search_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let query = required(args, "query")?;
    if query.is_empty() {
        bail!("query must be non-empty");
    }
    let rel = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let base = resolve_dir(&ws, rel)?;
    let deadline = Instant::now() + SEARCH_DEADLINE;
    let mut scanned = 0usize;
    let mut results = Vec::new();
    for entry in WalkDir::new(base)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_ignored_entry(entry))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() || !contains(&ws, entry.path()) {
            continue;
        }
        scanned += 1;
        if scanned > MAX_SEARCH_FILES || Instant::now() > deadline {
            return Ok(json!({"results": results, "truncated": true}));
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for (idx, line) in text.lines().enumerate() {
            if line.contains(query) {
                let path = entry.path().strip_prefix(&ws.root).unwrap_or(entry.path());
                results.push(json!({
                    "path": path.to_string_lossy(),
                    "line": idx + 1,
                    "text": truncate_string(line.trim(), 200)
                }));
                if results.len() >= MAX_SEARCH_RESULTS {
                    return Ok(json!({"results": results, "truncated": true}));
                }
            }
        }
    }
    Ok(json!({"results": results, "truncated": false}))
}

fn git_status_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let branch = git(&ws.root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head = git(&ws.root, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string();
    let status = git(&ws.root, &["status", "--short"])?;
    Ok(json!({"branch": branch, "head": head, "status": status}))
}

fn git_diff_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let stat = git(&ws.root, &["diff", "--stat"])?;
    let raw_diff = git_limited(&ws.root, &["diff"], MAX_READ_BYTES + 1)?;
    let truncated = raw_diff.len() > MAX_READ_BYTES;
    let diff = truncate_bytes(&raw_diff);
    Ok(json!({"stat": stat, "diff": diff, "truncated": truncated}))
}

fn show_changes_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    current_session_id: Option<&str>,
) -> Result<Value> {
    let workspace_id = required(args, "workspace_id")?;
    let ws = workspace(state, workspace_id)?;
    let branch = git(&ws.root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head = git(&ws.root, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string();
    let status = git(&ws.root, &["status", "--short"])?;
    let stat = git(&ws.root, &["diff", "--stat"])?;
    let raw_diff = git_limited(&ws.root, &["diff"], MAX_READ_BYTES + 1)?;
    let truncated = raw_diff.len() > MAX_READ_BYTES;
    let diff = truncate_bytes(&raw_diff);
    let recent_actions = current_session_id
        .and_then(|session_id| recent_change_actions(state, session_id, workspace_id).ok())
        .unwrap_or_default();
    Ok(json!({
        "workspace_id": workspace_id,
        "branch": branch,
        "head": head,
        "status": status,
        "stat": stat,
        "diff": diff,
        "truncated": truncated,
        "recent_actions": recent_actions
    }))
}

fn create_note_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    session_id: Option<&str>,
) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for review notes");
    };
    let workspace_id = required(args, "workspace_id")?;
    let _ = workspace(state, workspace_id)?;
    let title = required(args, "title")?;
    ensure_short_text("title", title, 200)?;
    let body = required(args, "body")?;
    ensure_short_text("body", body, MAX_REVIEW_NOTE_BODY_BYTES)?;
    let severity = args
        .get("severity")
        .and_then(Value::as_str)
        .unwrap_or("info");
    ensure_review_severity(severity)?;
    let path = optional_short_text(args, "path", 240)?;
    if let Some(path) = &path {
        ensure_workspace_relative_path_arg(path)?;
    }
    let note_id = Uuid::new_v4().simple().to_string();
    let entry = json!({
        "note_id": note_id,
        "ts_unix_ms": unix_ms(),
        "session_id": session_id,
        "workspace_id": workspace_id,
        "title": title,
        "severity": severity,
        "path": path,
        "body": body
    });
    append_review_note(state_dir, &entry)?;
    Ok(json!({
        "note_id": note_id,
        "workspace_id": workspace_id,
        "title": title,
        "severity": severity,
        "path": path,
        "body_chars": body.len()
    }))
}

fn list_notes_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for review notes");
    };
    if state.config.owner_token.is_none() {
        bail!("list_notes requires authenticated connector access");
    }
    let workspace_id = required(args, "workspace_id")?;
    ensure_short_text("workspace_id", workspace_id, 120)?;
    let _ = workspace(state, workspace_id)?;
    let severity = optional_short_text(args, "severity", 20)?;
    if let Some(severity) = &severity {
        ensure_review_severity(severity)?;
    }
    let path_filter = optional_short_text(args, "path", 240)?;
    if let Some(path) = &path_filter {
        ensure_workspace_relative_path_arg(path)?;
    }
    let limit = optional_u64(args, "limit")?.unwrap_or(50) as usize;
    let (notes, truncated) = read_review_notes(
        state_dir,
        Some(workspace_id),
        severity.as_deref(),
        path_filter.as_deref(),
        limit,
    )?;
    Ok(json!({
        "notes": notes,
        "truncated": truncated
    }))
}

fn show_review_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let workspace_id = required(args, "workspace_id")?;
    let limit = optional_u64(args, "limit")?.unwrap_or(25) as usize;
    let mut note_args = serde_json::Map::new();
    note_args.insert("workspace_id".to_string(), json!(workspace_id));
    note_args.insert("limit".to_string(), json!(limit));
    if let Some(severity) = args.get("severity") {
        note_args.insert("severity".to_string(), severity.clone());
    }
    if let Some(path) = args.get("path") {
        note_args.insert("path".to_string(), path.clone());
    }
    let notes_payload = list_notes_tool(state, &note_args)?;
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for review handoff");
    };
    let (edit_plans, edit_plans_truncated) =
        persisted_edit_plans(state_dir, Some(workspace_id), None, limit)?;
    Ok(json!({
        "workspace_id": workspace_id,
        "notes": notes_payload.get("notes").cloned().unwrap_or_else(|| json!([])),
        "edit_plans": edit_plans,
        "truncated": notes_payload.get("truncated").and_then(Value::as_bool).unwrap_or(false) || edit_plans_truncated
    }))
}

fn edit_plan_paths(ws: &Workspace, args: &serde_json::Map<String, Value>) -> Result<Vec<String>> {
    let Some(paths) = args.get("paths").and_then(Value::as_array) else {
        bail!("missing required argument: paths");
    };
    if paths.is_empty() {
        bail!("paths must include at least one workspace-relative path");
    }
    if paths.len() > MAX_EDIT_PLAN_PATHS {
        bail!("paths exceeds maximum path count");
    }
    let mut out = Vec::new();
    for path in paths {
        let Some(path) = path.as_str() else {
            bail!("paths must contain only strings");
        };
        ensure_workspace_relative_path_arg(path)?;
        match resolve_file(ws, path) {
            Ok(_) => {}
            Err(file_err) => {
                resolve_write_path(ws, path)
                    .with_context(|| format!("invalid edit plan path {path}: {file_err}"))?;
            }
        }
        out.push(truncate_string(path, 240));
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn patch_file_summaries(prepared: &[PreparedPatchFile]) -> Vec<PersistedPatchFileSummary> {
    prepared
        .iter()
        .map(|file| PersistedPatchFileSummary {
            path: file.path.clone(),
            operation: match file.operation {
                PatchOperation::Add => "add",
                PatchOperation::Modify => "modify",
                PatchOperation::Delete => "delete",
            }
            .to_string(),
            bytes_before: file.before.len(),
            bytes_after: file.after.as_deref().unwrap_or("").len(),
        })
        .collect()
}

fn create_edit_plan_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    session_id: Option<&str>,
) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for edit plans");
    };
    if state.persisted_state.is_none() {
        bail!("persisted state is not available for edit plans");
    }
    let workspace_id = required(args, "workspace_id")?;
    let ws = workspace(state, workspace_id)?;
    let title = required(args, "title")?;
    ensure_short_text("title", title, 200)?;
    let intent = required(args, "intent")?;
    ensure_short_text("intent", intent, MAX_EDIT_PLAN_INTENT_BYTES)?;
    let paths = edit_plan_paths(&ws, args)?;
    let patch = args.get("patch").and_then(Value::as_str);
    let (patch_chars, files) = if let Some(patch) = patch {
        let prepared = prepare_patch_files(&ws, patch, true)?;
        (Some(patch.len()), patch_file_summaries(&prepared))
    } else {
        (None, vec![])
    };
    let plan_id = Uuid::new_v4().simple().to_string();
    let now = unix_ms();
    let plan = PersistedEditPlan {
        created_unix_ms: now,
        updated_unix_ms: now,
        session_id: session_id.map(str::to_string),
        workspace_id: workspace_id.to_string(),
        plan_id: plan_id.clone(),
        status: default_edit_plan_status(),
        title: title.to_string(),
        intent: intent.to_string(),
        paths,
        patch_chars,
        files,
        status_note: None,
        approved_unix_ms: None,
        applied_unix_ms: None,
        applied_session_id: None,
        applied_files: vec![],
    };
    update_persisted_state(state, |snapshot| {
        snapshot.edit_plans.push(plan.clone());
    });
    if !state_dir.exists() {
        fs::create_dir_all(state_dir)?;
    }
    Ok(json!({
        "plan_id": plan_id,
        "workspace_id": workspace_id,
        "status": plan.status,
        "title": title,
        "intent_chars": intent.len(),
        "paths": plan.paths,
        "patch_chars": plan.patch_chars,
        "files": plan.files
    }))
}

fn list_edit_plans_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for edit plan state");
    };
    let workspace_id = args.get("workspace_id").and_then(Value::as_str);
    if let Some(workspace_id) = workspace_id {
        ensure_short_text("workspace_id", workspace_id, 120)?;
    }
    let status = args.get("status").and_then(Value::as_str);
    if let Some(status) = status {
        ensure_edit_plan_filter_status(status)?;
    }
    let limit = optional_u64(args, "limit")?.unwrap_or(50) as usize;
    let (edit_plans, truncated) = persisted_edit_plans(state_dir, workspace_id, status, limit)?;
    Ok(json!({
        "edit_plans": edit_plans,
        "truncated": truncated
    }))
}

fn show_edit_plans_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let listed = list_edit_plans_tool(state, args)?;
    let edit_plans = listed
        .get("edit_plans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut status_counts: HashMap<String, usize> = HashMap::new();
    for plan in &edit_plans {
        let status = plan
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *status_counts.entry(status.to_string()).or_insert(0) += 1;
    }
    Ok(json!({
        "workspace_id": args.get("workspace_id").and_then(Value::as_str),
        "status": args.get("status").and_then(Value::as_str),
        "edit_plans": edit_plans,
        "status_counts": status_counts,
        "truncated": listed.get("truncated").and_then(Value::as_bool).unwrap_or(false)
    }))
}

fn ensure_edit_plan_filter_status(value: &str) -> Result<()> {
    ensure_short_text("status", value, 20)?;
    if !matches!(value, "draft" | "approved" | "superseded" | "applied") {
        bail!("status must be one of draft, approved, superseded, or applied");
    }
    Ok(())
}

fn ensure_edit_plan_update_status(value: &str) -> Result<()> {
    ensure_short_text("status", value, 20)?;
    if !matches!(value, "draft" | "approved" | "superseded") {
        bail!("status must be one of draft, approved, or superseded");
    }
    Ok(())
}

fn update_edit_plan_status_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
) -> Result<Value> {
    let plan_id = required(args, "plan_id")?;
    ensure_short_text("plan_id", plan_id, 120)?;
    let status = required(args, "status")?;
    ensure_edit_plan_update_status(status)?;
    let status_note = optional_short_text(args, "status_note", 500)?;
    let plan = update_persisted_edit_plan(state, plan_id, |plan| {
        if plan.status == "applied" {
            bail!("applied edit plans cannot be moved back to {status}");
        }
        plan.status = status.to_string();
        plan.status_note = status_note.clone();
        if status == "approved" {
            plan.approved_unix_ms = Some(unix_ms());
        }
        Ok(())
    })?;
    Ok(json!({
        "plan_id": plan.plan_id,
        "workspace_id": plan.workspace_id,
        "status": plan.status,
        "title": plan.title,
        "status_note": plan.status_note,
        "approved_unix_ms": plan.approved_unix_ms,
        "updated_unix_ms": plan.updated_unix_ms
    }))
}

fn validate_edit_plan_for_apply(
    state: &AppState,
    plan_id: &str,
    workspace_id: &str,
    prepared: &[PreparedPatchFile],
) -> Result<()> {
    let Some(persisted) = &state.persisted_state else {
        bail!("persisted state is not available for edit plans");
    };
    let guard = persisted.lock().unwrap();
    let Some(plan) = guard
        .edit_plans
        .iter()
        .find(|edit_plan| edit_plan.plan_id == plan_id)
    else {
        bail!("edit plan not found: {plan_id}");
    };
    if plan.workspace_id != workspace_id {
        bail!("edit plan workspace does not match apply_patch workspace");
    }
    if plan.status != "approved" {
        bail!("edit plan must be approved before apply_patch");
    }
    let summaries = patch_file_summaries(prepared);
    if plan.files.is_empty() {
        bail!("edit plan has no validated patch summary for apply_patch");
    }
    if plan.files != summaries {
        bail!("apply_patch file summary does not match edit plan");
    }
    Ok(())
}

fn mark_edit_plan_applied(
    state: &AppState,
    plan_id: &str,
    session_id: Option<&str>,
    prepared: &[PreparedPatchFile],
) -> Result<PersistedEditPlan> {
    let applied_files = patch_file_summaries(prepared);
    update_persisted_edit_plan(state, plan_id, |plan| {
        plan.status = "applied".to_string();
        plan.applied_unix_ms = Some(unix_ms());
        plan.applied_session_id = session_id.map(str::to_string);
        plan.applied_files = applied_files;
        Ok(())
    })
}

fn write_file_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let rel = required(args, "path")?;
    let content = required(args, "content")?;
    if content.len() > MAX_WRITE_BYTES {
        bail!("content exceeds maximum write size");
    }
    ensure_text_content(content)?;
    let target = resolve_write_path(&ws, rel)?;
    let before = read_existing_text_for_mutation(&target)?;
    let before_bytes = before.as_ref().map(|text| text.len()).unwrap_or(0);
    fs::write(&target, content.as_bytes())?;
    let (diff, truncated) = bounded_text_diff(before.as_deref().unwrap_or(""), content);
    Ok(json!({
        "path": rel,
        "created": before.is_none(),
        "bytes_before": before_bytes,
        "bytes_after": content.len(),
        "diff": diff,
        "truncated": truncated,
    }))
}

fn edit_file_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let rel = required(args, "path")?;
    let old_text = required(args, "old_text")?;
    let new_text = required(args, "new_text")?;
    let replace_all = optional_bool(args, "replace_all")?.unwrap_or(false);
    let expected_replacements = optional_u64(args, "expected_replacements")?;
    if new_text.len() > MAX_WRITE_BYTES {
        bail!("new_text exceeds maximum write size");
    }
    ensure_text_content(new_text)?;
    if old_text.is_empty() {
        bail!("old_text must be non-empty");
    }
    ensure_text_content(old_text)?;
    let target = resolve_file(&ws, rel)?;
    let metadata = fs::metadata(&target)?;
    if metadata.len() as usize > MAX_WRITE_BYTES {
        bail!("file exceeds maximum edit size");
    }
    let before_bytes = fs::read(&target)?;
    let before = String::from_utf8(before_bytes).map_err(|_| anyhow!("file is not valid UTF-8"))?;
    ensure_text_content(&before)?;
    let matches = before.matches(old_text).count();
    if matches == 0 {
        bail!("old_text was not found");
    }
    if !replace_all && matches > 1 {
        bail!(
            "old_text matched multiple locations; set replace_all or use a more specific old_text"
        );
    }
    if let Some(expected) = expected_replacements {
        if matches as u64 != expected {
            bail!("replacement count mismatch: expected {expected}, found {matches}");
        }
    }
    let after = if replace_all {
        before.replace(old_text, new_text)
    } else {
        before.replacen(old_text, new_text, 1)
    };
    if after.len() > MAX_WRITE_BYTES {
        bail!("edited content exceeds maximum write size");
    }
    fs::write(&target, after.as_bytes())?;
    let (diff, truncated) = bounded_text_diff(&before, &after);
    Ok(json!({
        "path": rel,
        "replacements": if replace_all { matches } else { 1 },
        "bytes_before": before.len(),
        "bytes_after": after.len(),
        "diff": diff,
        "truncated": truncated,
    }))
}

fn move_path_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let from_rel = required(args, "from_path")?;
    let to_rel = required(args, "to_path")?;
    let overwrite = optional_bool(args, "overwrite")?.unwrap_or(false);
    if from_rel == to_rel {
        bail!("from_path and to_path must differ");
    }
    let from = resolve_file(&ws, from_rel)?;
    let to = resolve_write_path(&ws, to_rel)?;
    let from_meta = fs::metadata(&from)?;
    if from_meta.len() as usize > MAX_WRITE_BYTES {
        bail!("file exceeds maximum move size");
    }
    let mut overwritten = false;
    match fs::symlink_metadata(&to) {
        Ok(meta) if meta.file_type().is_symlink() => bail!("destination path is a symlink"),
        Ok(meta) if meta.is_dir() => bail!("destination path is a directory"),
        Ok(meta) if meta.is_file() => {
            if !overwrite {
                bail!("destination already exists; set overwrite=true to replace it");
            }
            overwritten = true;
        }
        Ok(_) => bail!("destination path exists and is not a regular file"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    fs::rename(&from, &to)?;
    Ok(json!({
        "from_path": from_rel,
        "to_path": to_rel,
        "overwritten": overwritten,
        "bytes": from_meta.len(),
    }))
}

fn prepare_patch_files(
    ws: &Workspace,
    patch: &str,
    add_targets_must_not_exist: bool,
) -> Result<Vec<PreparedPatchFile>> {
    if patch.len() > MAX_PATCH_BYTES {
        bail!("patch exceeds maximum patch size");
    }
    ensure_text_content(patch)?;
    let files = parse_unified_patch(patch)?;
    if files.is_empty() {
        bail!("patch contains no files");
    }
    let mut prepared: Vec<PreparedPatchFile> = Vec::new();
    for file in files {
        let target = match file.operation {
            PatchOperation::Add => resolve_write_path(ws, &file.path)?,
            PatchOperation::Modify | PatchOperation::Delete => resolve_file(ws, &file.path)?,
        };
        if add_targets_must_not_exist
            && matches!(file.operation, PatchOperation::Add)
            && target.exists()
        {
            bail!("target file already exists: {}", file.path);
        }
        let before = if file.operation == PatchOperation::Add {
            String::new()
        } else {
            let metadata = fs::metadata(&target)?;
            if metadata.len() as usize > MAX_WRITE_BYTES {
                bail!("file exceeds maximum edit size: {}", file.path);
            }
            let before_bytes = fs::read(&target)?;
            let before =
                String::from_utf8(before_bytes).map_err(|_| anyhow!("file is not valid UTF-8"))?;
            ensure_text_content(&before)?;
            before
        };
        let after = apply_hunks_to_text(&before, &file)?;
        if file.operation == PatchOperation::Delete && !after.is_empty() {
            bail!("delete patch leaves content for {}", file.path);
        }
        if after.len() > MAX_WRITE_BYTES {
            bail!("patched content exceeds maximum write size: {}", file.path);
        }
        prepared.push(PreparedPatchFile {
            path: file.path,
            target,
            before,
            after: (file.operation != PatchOperation::Delete).then_some(after),
            operation: file.operation,
        });
    }
    Ok(prepared)
}

fn patch_preview_payload(prepared: &[PreparedPatchFile]) -> Value {
    let mut changed_files = Vec::new();
    let mut combined_diff = String::new();
    let mut truncated = false;
    for file in prepared {
        let after_for_diff = file.after.as_deref().unwrap_or("");
        let (diff, diff_truncated) = bounded_text_diff(&file.before, after_for_diff);
        truncated |= diff_truncated;
        if combined_diff.len() < MAX_DIFF_BYTES {
            combined_diff.push_str("--- ");
            combined_diff.push_str(&file.path);
            combined_diff.push('\n');
            combined_diff.push_str(&diff);
        } else {
            truncated = true;
        }
        changed_files.push(json!({
            "path": file.path,
            "operation": match file.operation {
                PatchOperation::Add => "add",
                PatchOperation::Modify => "modify",
                PatchOperation::Delete => "delete",
            },
            "bytes_before": file.before.len(),
            "bytes_after": after_for_diff.len()
        }));
    }
    if combined_diff.len() > MAX_DIFF_BYTES {
        combined_diff.truncate(MAX_DIFF_BYTES);
        truncated = true;
    }
    json!({
        "files": changed_files,
        "diff": combined_diff,
        "truncated": truncated,
    })
}

fn preview_patch_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let patch = required(args, "patch")?;
    let prepared = prepare_patch_files(&ws, patch, true)?;
    let mut payload = patch_preview_payload(&prepared);
    if let Some(map) = payload.as_object_mut() {
        map.insert("would_apply".to_string(), json!(true));
    }
    Ok(payload)
}

fn apply_patch_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    session_id: Option<&str>,
) -> Result<Value> {
    let workspace_id = required(args, "workspace_id")?;
    let ws = workspace(state, workspace_id)?;
    let patch = required(args, "patch")?;
    let prepared = prepare_patch_files(&ws, patch, true)?;
    let plan_id = optional_short_text(args, "plan_id", 120)?;
    if let Some(plan_id) = &plan_id {
        validate_edit_plan_for_apply(state, plan_id, workspace_id, &prepared)?;
    }
    for file in &prepared {
        if let Some(after) = &file.after {
            fs::write(&file.target, after.as_bytes())?;
        } else {
            fs::remove_file(&file.target)?;
        }
    }
    let mut payload = patch_preview_payload(&prepared);
    if let Some(plan_id) = &plan_id {
        let plan = mark_edit_plan_applied(state, plan_id, session_id, &prepared)?;
        if let Some(map) = payload.as_object_mut() {
            map.insert("plan_id".to_string(), json!(plan.plan_id));
            map.insert("plan_status".to_string(), json!(plan.status));
        }
    }
    Ok(payload)
}

fn parse_unified_patch(patch: &str) -> Result<Vec<ParsedPatchFile>> {
    let lines: Vec<&str> = patch.lines().collect();
    let mut idx = 0usize;
    let mut files = Vec::new();
    while idx < lines.len() {
        if !lines[idx].starts_with("--- ") {
            idx += 1;
            continue;
        }
        let old_path = lines[idx].trim_start_matches("--- ").trim();
        idx += 1;
        if idx >= lines.len() || !lines[idx].starts_with("+++ ") {
            bail!("patch file header missing +++ for {old_path}");
        }
        let new_path = lines[idx].trim_start_matches("+++ ").trim();
        idx += 1;
        let old_path = normalize_patch_path(old_path);
        let new_path = normalize_patch_path(new_path);
        let (path, operation) = match (old_path, new_path) {
            (None, Some(path)) => (path, PatchOperation::Add),
            (Some(path), None) => (path, PatchOperation::Delete),
            (Some(old_path), Some(new_path)) => {
                if old_path != new_path {
                    bail!("renames are not supported: {old_path} -> {new_path}");
                }
                (new_path, PatchOperation::Modify)
            }
            (None, None) => bail!("patch path may not be /dev/null on both sides"),
        };
        let mut hunks = Vec::new();
        while idx < lines.len() {
            if lines[idx].starts_with("--- ") {
                break;
            }
            if !lines[idx].starts_with("@@ ") {
                if lines[idx].trim().is_empty() {
                    idx += 1;
                    continue;
                }
                bail!("expected hunk header for {path}");
            }
            let old_start = parse_hunk_old_start(lines[idx])?;
            idx += 1;
            let mut hunk_lines = Vec::new();
            while idx < lines.len()
                && !lines[idx].starts_with("@@ ")
                && !lines[idx].starts_with("--- ")
            {
                let line = lines[idx];
                if line.starts_with("\\ No newline") {
                    idx += 1;
                    continue;
                }
                if line.is_empty() {
                    bail!("invalid patch line");
                }
                let (prefix, text) = line.split_at(1);
                match prefix {
                    " " => hunk_lines.push(PatchLine::Context(text.to_string())),
                    "-" => hunk_lines.push(PatchLine::Remove(text.to_string())),
                    "+" => hunk_lines.push(PatchLine::Add(text.to_string())),
                    _ => bail!("invalid patch line prefix: {prefix}"),
                }
                idx += 1;
            }
            hunks.push(ParsedHunk {
                old_start,
                lines: hunk_lines,
            });
        }
        if hunks.is_empty() {
            bail!("patch file has no hunks: {path}");
        }
        files.push(ParsedPatchFile {
            path,
            operation,
            hunks,
        });
    }
    Ok(files)
}

fn normalize_patch_path(raw: &str) -> Option<String> {
    if raw == "/dev/null" {
        return None;
    }
    let path = raw.split_whitespace().next().unwrap_or(raw);
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn parse_hunk_old_start(header: &str) -> Result<usize> {
    let Some(rest) = header.strip_prefix("@@ -") else {
        bail!("invalid hunk header");
    };
    let Some((range, _)) = rest.split_once(' ') else {
        bail!("invalid hunk header");
    };
    let start = range
        .split(',')
        .next()
        .ok_or_else(|| anyhow!("invalid hunk old range"))?
        .parse::<usize>()
        .with_context(|| "invalid hunk old start")?;
    Ok(start.max(1))
}

fn apply_hunks_to_text(before: &str, file: &ParsedPatchFile) -> Result<String> {
    let source_lines = split_patch_lines(before);
    let mut output = Vec::new();
    let mut cursor = 0usize;
    for hunk in &file.hunks {
        let hunk_index = hunk.old_start.saturating_sub(1);
        if hunk_index < cursor || hunk_index > source_lines.len() {
            bail!("hunk location is invalid for {}", file.path);
        }
        output.extend_from_slice(&source_lines[cursor..hunk_index]);
        cursor = hunk_index;
        for line in &hunk.lines {
            match line {
                PatchLine::Context(text) => {
                    let Some(existing) = source_lines.get(cursor) else {
                        bail!("context line is past end of file: {}", file.path);
                    };
                    if normalize_patch_line(existing) != text {
                        bail!("context mismatch while patching {}", file.path);
                    }
                    output.push(existing.clone());
                    cursor += 1;
                }
                PatchLine::Remove(text) => {
                    let Some(existing) = source_lines.get(cursor) else {
                        bail!("remove line is past end of file: {}", file.path);
                    };
                    if normalize_patch_line(existing) != text {
                        bail!("remove mismatch while patching {}", file.path);
                    }
                    cursor += 1;
                }
                PatchLine::Add(text) => {
                    output.push(format!("{text}\n"));
                }
            }
        }
    }
    output.extend_from_slice(&source_lines[cursor..]);
    Ok(output.concat())
}

fn split_patch_lines(value: &str) -> Vec<String> {
    if value.is_empty() {
        return vec![];
    }
    value
        .split_inclusive('\n')
        .map(ToString::to_string)
        .collect()
}

fn normalize_patch_line(value: &str) -> &str {
    value.trim_end_matches('\n').trim_end_matches('\r')
}

fn shell_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let command = required(args, "command")?;
    if command.trim().is_empty() {
        bail!("command must be non-empty");
    }
    ensure_text_content(command)?;
    let cwd_rel = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
    let cwd = resolve_dir(&ws, cwd_rel)?;
    let timeout_ms = optional_u64(args, "timeout_ms")?.unwrap_or(SHELL_TIMEOUT.as_millis() as u64);
    let timeout = Duration::from_millis(timeout_ms.min(SHELL_TIMEOUT.as_millis() as u64).max(1));
    let mut child = Command::new("/bin/bash")
        .arg("-lc")
        .arg(command)
        .current_dir(&cwd)
        .env_clear()
        .envs(safe_shell_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "failed to spawn /bin/bash")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("shell stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("shell stderr unavailable"))?;
    let stdout_reader = thread::spawn(move || read_pipe_limited(stdout, MAX_SHELL_OUTPUT_BYTES));
    let stderr_reader = thread::spawn(move || read_pipe_limited(stderr, MAX_SHELL_OUTPUT_BYTES));
    let start = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let status = child.wait().ok();
            break (status, true);
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("shell stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("shell stderr reader panicked"))??;
    let stdout_truncated = stdout.len() > MAX_SHELL_OUTPUT_BYTES;
    let stderr_truncated = stderr.len() > MAX_SHELL_OUTPUT_BYTES;
    let mut stdout = stdout;
    let mut stderr = stderr;
    stdout.truncate(MAX_SHELL_OUTPUT_BYTES);
    stderr.truncate(MAX_SHELL_OUTPUT_BYTES);
    Ok(json!({
        "cwd": cwd.strip_prefix(&ws.root).unwrap_or(Path::new(".")).to_string_lossy(),
        "exit_code": status.and_then(|status| status.code()),
        "timed_out": timed_out,
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "truncated": stdout_truncated || stderr_truncated,
    }))
}

fn open_worktree_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for managed worktrees");
    };
    let source_workspace_id = required(args, "workspace_id")?;
    let ws = workspace(state, source_workspace_id)?;
    let repo_root = git(&ws.root, &["rev-parse", "--show-toplevel"])?
        .trim()
        .to_string();
    let repo_root = PathBuf::from(repo_root).canonicalize()?;
    if repo_root != ws.root {
        bail!("open_worktree requires the workspace root to be the git repository root");
    }
    let worktree_root = worktrees_root(state_dir);
    fs::create_dir_all(&worktree_root)?;
    if managed_worktrees(state_dir)?.len() >= MAX_WORKTREES {
        bail!("managed worktree limit reached");
    }
    let id = Uuid::new_v4().simple().to_string();
    let branch = args
        .get("branch")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("codex/worktree-{}", &id[..12]));
    validate_branch_name(&ws.root, &branch)?;
    let base_ref = args
        .get("base_ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD");
    validate_git_ref_arg(base_ref)?;
    validate_base_ref(&ws.root, base_ref)?;
    let task_id = optional_short_text(args, "task_id", 120)?;
    let task = optional_short_text(args, "task", 500)?;
    let target = worktree_root.join(&id);
    let status = Command::new("git")
        .args(["worktree", "add", "-b", &branch])
        .arg(&target)
        .arg(base_ref)
        .current_dir(&ws.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| "git is not installed")?;
    if !status.success() {
        bail!("git worktree add failed");
    }
    let canonical = target.canonicalize()?;
    write_worktree_metadata(
        state_dir,
        &id,
        &ManagedWorktreeMetadata {
            created_unix_ms: unix_ms(),
            source_workspace_id: Some(source_workspace_id.to_string()),
            branch: Some(branch.clone()),
            base_ref: Some(base_ref.to_string()),
            task_id: task_id.clone(),
            task: task.clone(),
        },
    )?;
    let (workspace_id, name, _) = register_workspace_root(state, canonical);
    Ok(json!({
        "workspace_id": workspace_id,
        "name": name,
        "branch": branch,
        "base_ref": base_ref,
        "task_id": task_id,
        "task": task,
    }))
}

fn list_worktrees_tool(state: &AppState) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for managed worktrees");
    };
    let worktrees = managed_worktrees(state_dir)?;
    let truncated = worktrees.len() > MAX_WORKTREES;
    let mut items = Vec::new();
    for worktree in worktrees.into_iter().take(MAX_WORKTREES) {
        let managed_name = worktree
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("worktree")
            .to_string();
        let canonical = match worktree.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                items.push(json!({
                    "managed_name": managed_name,
                    "available": false,
                    "error": truncate_string(&err.to_string(), 200)
                }));
                continue;
            }
        };
        let (workspace_id, name, _) = register_workspace_root(state, canonical.clone());
        let branch = git(&canonical, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let head = git(&canonical, &["rev-parse", "--short", "HEAD"])
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        let status = git(&canonical, &["status", "--short"]).unwrap_or_default();
        let metadata = read_worktree_metadata(state_dir, &managed_name).unwrap_or_default();
        items.push(json!({
            "workspace_id": workspace_id,
            "name": name,
            "managed_name": managed_name,
            "branch": branch,
            "head": head,
            "status": status,
            "task_id": metadata.task_id,
            "task": metadata.task,
            "available": true
        }));
    }
    Ok(json!({"worktrees": items, "truncated": truncated}))
}

fn list_pull_requests_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for pull request state");
    };
    let workspace_id = args.get("workspace_id").and_then(Value::as_str);
    let branch = args.get("branch").and_then(Value::as_str);
    if let Some(workspace_id) = workspace_id {
        ensure_short_text("workspace_id", workspace_id, 120)?;
    }
    if let Some(branch) = branch {
        ensure_short_text("branch", branch, 200)?;
    }
    let limit = optional_u64(args, "limit")?.unwrap_or(50) as usize;
    let (pull_requests, truncated) =
        persisted_pull_requests(state_dir, workspace_id, branch, limit)?;
    Ok(json!({
        "pull_requests": pull_requests,
        "truncated": truncated
    }))
}

fn show_pull_requests_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
) -> Result<Value> {
    let listed = list_pull_requests_tool(state, args)?;
    let pull_requests = listed
        .get("pull_requests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut status_counts: HashMap<String, usize> = HashMap::new();
    for pull_request in &pull_requests {
        let status = if pull_request
            .get("merged")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "merged"
        } else {
            pull_request
                .get("remote_state")
                .and_then(Value::as_str)
                .or_else(|| pull_request.get("status").and_then(Value::as_str))
                .unwrap_or("unknown")
        };
        *status_counts.entry(status.to_string()).or_insert(0) += 1;
    }
    Ok(json!({
        "workspace_id": args.get("workspace_id").and_then(Value::as_str),
        "branch": args.get("branch").and_then(Value::as_str),
        "pull_requests": pull_requests,
        "status_counts": status_counts,
        "truncated": listed.get("truncated").and_then(Value::as_bool).unwrap_or(false)
    }))
}

fn publish_branch_tool(state: &AppState, args: &serde_json::Map<String, Value>) -> Result<Value> {
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let branch = git(&ws.root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if branch == "HEAD" {
        bail!("cannot publish a detached HEAD");
    }
    validate_branch_name(&ws.root, &branch)?;
    let remote = args
        .get("remote")
        .and_then(Value::as_str)
        .unwrap_or("origin");
    validate_git_remote_arg(remote)?;
    let remote_branch = args
        .get("remote_branch")
        .and_then(Value::as_str)
        .unwrap_or(&branch);
    validate_branch_name(&ws.root, remote_branch)?;
    let refspec = format!("{branch}:{remote_branch}");
    let output = git_push_with_output(&ws.root, remote, &refspec)?;
    Ok(json!({
        "branch": branch,
        "remote": remote,
        "remote_branch": remote_branch,
        "exit_code": output.exit_code,
        "success": output.exit_code == Some(0) && !output.timed_out,
        "timed_out": output.timed_out,
        "stdout": scrub_git_output(&String::from_utf8_lossy(&output.stdout)),
        "stderr": scrub_git_output(&String::from_utf8_lossy(&output.stderr)),
        "truncated": output.truncated,
    }))
}

fn create_pull_request_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
) -> Result<Value> {
    create_pull_request_tool_with_gh(state, args, "gh")
}

fn create_pull_request_tool_with_gh(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    gh_bin: &str,
) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for pull request handoff");
    };
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let branch = git(&ws.root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if branch == "HEAD" {
        bail!("cannot create a pull request from a detached HEAD");
    }
    validate_branch_name(&ws.root, &branch)?;
    let title = required(args, "title")?;
    ensure_short_text("title", title, 200)?;
    let body = required(args, "body")?;
    ensure_short_text("body", body, MAX_PR_BODY_BYTES)?;
    let base = optional_short_text(args, "base", 120)?;
    if let Some(base) = &base {
        validate_git_ref_arg(base)?;
    }
    let draft = optional_bool(args, "draft")?.unwrap_or(false);
    let body_path = write_pr_body_file(state_dir, body)?;
    let body_path_str = body_path.to_string_lossy().to_string();
    let mut gh_args = vec![
        "pr".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
        "--body-file".to_string(),
        body_path_str,
        "--head".to_string(),
        branch.clone(),
    ];
    if let Some(base) = &base {
        gh_args.push("--base".to_string());
        gh_args.push(base.clone());
    }
    if draft {
        gh_args.push("--draft".to_string());
    }
    let output = command_with_output(gh_bin, &gh_args, &ws.root, GH_TIMEOUT)
        .with_context(|| "failed to run gh pr create")?;
    let stdout = scrub_git_output(&String::from_utf8_lossy(&output.stdout));
    let stderr = scrub_git_output(&String::from_utf8_lossy(&output.stderr));
    let url = extract_first_url(&stdout).or_else(|| extract_first_url(&stderr));
    Ok(json!({
        "branch": branch,
        "base": base,
        "title": title,
        "draft": draft,
        "exit_code": output.exit_code,
        "success": output.exit_code == Some(0) && !output.timed_out,
        "timed_out": output.timed_out,
        "url": url,
        "body_chars": body.len(),
        "stdout": stdout,
        "stderr": stderr,
        "truncated": output.truncated,
    }))
}

fn refresh_pull_request_status_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
) -> Result<Value> {
    refresh_pull_request_status_tool_with_gh(state, args, "gh")
}

fn refresh_pull_request_status_tool_with_gh(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    gh_bin: &str,
) -> Result<Value> {
    let Some(_state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for pull request state");
    };
    let ws = workspace(state, required(args, "workspace_id")?)?;
    let workspace_id = required(args, "workspace_id")?;
    ensure_short_text("workspace_id", workspace_id, 120)?;
    let requested_branch = optional_short_text(args, "branch", 200)?;
    let requested_url = optional_short_text(args, "url", 500)?;
    let selector = if let Some(url) = &requested_url {
        url.clone()
    } else if let Some(branch) = &requested_branch {
        branch.clone()
    } else {
        git(&ws.root, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string()
    };
    refresh_pull_request_selector_with_gh(
        state,
        workspace_id,
        &ws.root,
        PullRequestRefreshTarget {
            selector,
            requested_branch,
            requested_url,
            record_key: None,
        },
        gh_bin,
        GH_TIMEOUT,
    )
}

fn refresh_pull_requests_tool(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
) -> Result<Value> {
    refresh_pull_requests_tool_with_gh(state, args, "gh")
}

fn refresh_pull_requests_tool_with_gh(
    state: &AppState,
    args: &serde_json::Map<String, Value>,
    gh_bin: &str,
) -> Result<Value> {
    let Some(state_dir) = &state.config.state_dir else {
        bail!("state_dir is required for pull request state");
    };
    let workspace_id = required(args, "workspace_id")?;
    ensure_short_text("workspace_id", workspace_id, 120)?;
    let ws = workspace(state, workspace_id)?;
    let branch = optional_short_text(args, "branch", 200)?;
    let requested_limit =
        optional_u64(args, "limit")?.unwrap_or(MAX_PULL_REQUEST_REFRESHES as u64) as usize;
    let limit = requested_limit.min(MAX_PULL_REQUEST_REFRESHES);
    let (pull_requests, truncated) =
        persisted_pull_requests(state_dir, Some(workspace_id), branch.as_deref(), limit)?;
    let mut refreshed = Vec::with_capacity(pull_requests.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let deadline = Instant::now() + GH_BATCH_TIMEOUT;
    let mut deadline_reached = false;
    for pull_request in pull_requests {
        if Instant::now() >= deadline {
            deadline_reached = true;
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let requested_url = pull_request.url.clone();
        let requested_branch = Some(pull_request.branch.clone());
        let selector = requested_url
            .clone()
            .unwrap_or_else(|| pull_request.branch.clone());
        let record_key = PullRequestRecordKey {
            created_unix_ms: pull_request.created_unix_ms,
            branch: pull_request.branch.clone(),
            url: pull_request.url.clone(),
        };
        let result = refresh_pull_request_selector_with_gh(
            state,
            workspace_id,
            &ws.root,
            PullRequestRefreshTarget {
                selector: selector.clone(),
                requested_branch: requested_branch.clone(),
                requested_url: requested_url.clone(),
                record_key: Some(&record_key),
            },
            gh_bin,
            remaining.min(GH_TIMEOUT),
        )
        .unwrap_or_else(|err| {
            let match_branch = requested_url
                .is_none()
                .then_some(pull_request.branch.as_str());
            let match_url = requested_url.as_deref();
            let updated = update_persisted_pull_request_for_refresh(
                state,
                workspace_id,
                Some(&record_key),
                match_branch,
                match_url,
                |record| {
                    record.status = "refresh_failed".to_string();
                    record.remote_state = None;
                    record.merged = None;
                    record.number = None;
                    record.exit_code = None;
                    Ok(())
                },
            )
            .ok();
            json!({
                "success": false,
                "selector": selector,
                "exit_code": Value::Null,
                "timed_out": false,
                "stdout": "",
                "stderr": truncate_string(&err.to_string(), MAX_SHELL_OUTPUT_BYTES),
                "truncated": false,
                "pull_request": updated,
            })
        });
        if result
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            succeeded += 1;
        } else {
            failed += 1;
        }
        refreshed.push(result);
    }
    Ok(json!({
        "workspace_id": workspace_id,
        "branch": branch,
        "refreshed": refreshed,
        "succeeded": succeeded,
        "failed": failed,
        "truncated": truncated || deadline_reached
    }))
}

fn refresh_pull_request_selector_with_gh(
    state: &AppState,
    workspace_id: &str,
    root: &Path,
    target: PullRequestRefreshTarget<'_>,
    gh_bin: &str,
    timeout: Duration,
) -> Result<Value> {
    ensure_short_text("selector", &target.selector, 500)?;
    let gh_args = vec![
        "pr".to_string(),
        "view".to_string(),
        target.selector.clone(),
        "--json".to_string(),
        "state,merged,url,number,title,baseRefName,headRefName,isDraft".to_string(),
    ];
    let output = command_with_output(gh_bin, &gh_args, root, timeout)
        .with_context(|| "failed to run gh pr view")?;
    let stdout_raw = String::from_utf8_lossy(&output.stdout).to_string();
    let stdout = scrub_git_output(&stdout_raw);
    let stderr = scrub_git_output(&String::from_utf8_lossy(&output.stderr));
    let success = output.exit_code == Some(0) && !output.timed_out;
    if !success {
        let match_branch = target
            .requested_url
            .is_none()
            .then_some(target.selector.as_str());
        let match_url = target.requested_url.as_deref();
        let pull_request = update_persisted_pull_request_for_refresh(
            state,
            workspace_id,
            target.record_key,
            match_branch,
            match_url,
            |record| {
                record.status = "refresh_failed".to_string();
                record.remote_state = None;
                record.merged = None;
                record.number = None;
                record.exit_code = output.exit_code;
                Ok(())
            },
        )
        .ok();
        return Ok(json!({
            "success": false,
            "selector": target.selector,
            "exit_code": output.exit_code,
            "timed_out": output.timed_out,
            "stdout": stdout,
            "stderr": stderr,
            "truncated": output.truncated,
            "pull_request": pull_request,
        }));
    }
    let view: Value = serde_json::from_str(stdout_raw.trim())
        .with_context(|| "failed to parse gh pr view JSON output")?;
    let remote_state = view
        .get("state")
        .and_then(Value::as_str)
        .map(|value| value.to_ascii_lowercase());
    let merged = view.get("merged").and_then(Value::as_bool);
    let status = if merged == Some(true) {
        "merged".to_string()
    } else {
        remote_state
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
            .to_ascii_lowercase()
    };
    let head_branch = view
        .get("headRefName")
        .and_then(Value::as_str)
        .map(|value| truncate_string(value, 200))
        .or(target.requested_branch.clone())
        .unwrap_or_else(|| target.selector.clone());
    let refreshed_url = view
        .get("url")
        .and_then(Value::as_str)
        .map(|value| truncate_string(value, 500))
        .or(target.requested_url.clone());
    let number = view.get("number").and_then(Value::as_u64);
    let title = view
        .get("title")
        .and_then(Value::as_str)
        .map(|value| truncate_string(value, 200));
    let base = view
        .get("baseRefName")
        .and_then(Value::as_str)
        .map(|value| truncate_string(value, 200));
    let draft = view.get("isDraft").and_then(Value::as_bool);
    let match_branch = target
        .requested_url
        .is_none()
        .then_some(head_branch.as_str());
    let match_url = target.requested_url.as_deref();
    let pull_request = update_persisted_pull_request_for_refresh(
        state,
        workspace_id,
        target.record_key,
        match_branch,
        match_url,
        |pull_request| {
            pull_request.branch = head_branch.clone();
            pull_request.status = status.clone();
            pull_request.remote_state = remote_state.clone();
            pull_request.merged = merged;
            pull_request.number = number;
            if let Some(url) = &refreshed_url {
                pull_request.url = Some(url.clone());
            }
            if let Some(title) = &title {
                pull_request.title = title.clone();
            }
            if let Some(base) = &base {
                pull_request.base = Some(base.clone());
            }
            if let Some(draft) = draft {
                pull_request.draft = draft;
            }
            pull_request.exit_code = output.exit_code;
            Ok(())
        },
    )?;
    Ok(json!({
        "success": true,
        "selector": target.selector,
        "exit_code": output.exit_code,
        "timed_out": output.timed_out,
        "stdout": stdout,
        "stderr": stderr,
        "truncated": output.truncated,
        "pull_request": pull_request,
    }))
}

fn read_existing_text_for_mutation(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        bail!("path is a symlink");
    }
    if !meta.is_file() {
        bail!("not a file");
    }
    if meta.len() as usize > MAX_WRITE_BYTES {
        bail!("existing file exceeds maximum write size");
    }
    let bytes = fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|_| anyhow!("existing file is not valid UTF-8"))?;
    ensure_text_content(&text)?;
    Ok(Some(text))
}

fn workspace_instructions(root: &Path) -> Result<InstructionScan> {
    let mut loaded = Vec::new();
    let mut loaded_canonical = HashSet::new();
    for name in instruction_file_names() {
        let path = root.join(name);
        if !path.exists() {
            continue;
        }
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        let canonical = path.canonicalize()?;
        if !loaded_canonical.insert(canonical) {
            continue;
        }
        let (content, truncated) = read_bounded_text(&path, MAX_INSTRUCTION_BYTES)?;
        loaded.push(json!({
            "path": name,
            "content": content,
            "truncated": truncated,
        }));
    }

    let mut available_paths = Vec::new();
    let mut available_canonical = HashSet::new();
    let scan_deadline = Instant::now() + INSTRUCTION_SCAN_DEADLINE;
    let mut scanned = 0usize;
    let mut truncated = false;
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_ignored_entry(entry))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        scanned += 1;
        if scanned > MAX_INSTRUCTION_SCAN_ENTRIES || Instant::now() > scan_deadline {
            truncated = true;
            break;
        }
        if entry.depth() <= 1 || !entry.file_type().is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str() else {
            continue;
        };
        if !instruction_file_names().contains(&file_name) {
            continue;
        }
        let canonical = entry
            .path()
            .canonicalize()
            .unwrap_or_else(|_| entry.path().to_path_buf());
        if !is_contained(root, &canonical) || !available_canonical.insert(canonical) {
            continue;
        }
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        available_paths.push(rel.to_string_lossy().to_string());
        if available_paths.len() > MAX_AVAILABLE_INSTRUCTIONS {
            truncated = true;
            break;
        }
    }
    available_paths.sort();
    available_paths.truncate(MAX_AVAILABLE_INSTRUCTIONS);
    let available = available_paths
        .into_iter()
        .map(|path| json!({ "path": path }))
        .collect();
    Ok(InstructionScan {
        loaded,
        available,
        truncated,
    })
}

fn list_skills_tool(state: &AppState) -> Result<Value> {
    let (skills, truncated) = skill_summaries(&state.config, None)?;
    Ok(json!({"skills": skills, "truncated": truncated}))
}

fn skill_summaries(config: &Config, workspace_root: Option<&Path>) -> Result<(Vec<Value>, bool)> {
    let (skills, truncated) = discover_skills(config, workspace_root)?;
    Ok((
        skills
            .into_iter()
            .map(|skill| {
                json!({
                    "skill_id": skill.id,
                    "name": skill.name,
                    "description": skill.description,
                    "path": skill.path,
                    "entrypoint": skill.entrypoint,
                })
            })
            .collect(),
        truncated,
    ))
}

fn discover_skills(
    config: &Config,
    workspace_root: Option<&Path>,
) -> Result<(Vec<SkillEntry>, bool)> {
    let mut skills = Vec::new();
    let roots = effective_skill_roots(config, workspace_root);
    for root in &roots {
        collect_skill_entries(root, &mut skills)?;
        if skills.len() > MAX_SKILLS {
            break;
        }
    }
    skills.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    let truncated = skills.len() > MAX_SKILLS;
    skills.truncate(MAX_SKILLS);
    Ok((skills, truncated))
}

fn effective_skill_roots(config: &Config, workspace_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in &config.skill_roots {
        push_canonical_skill_root(&mut roots, root);
    }
    if !config.auto_skill_roots {
        return roots;
    }
    if let Some(workspace_root) = workspace_root {
        push_workspace_skill_root(
            &mut roots,
            workspace_root,
            &workspace_root.join(".pi/skills"),
        );
        push_workspace_skill_root(&mut roots, workspace_root, &workspace_root.join("skills"));
    }
    roots
}

fn push_canonical_skill_root(roots: &mut Vec<PathBuf>, candidate: &Path) {
    let Ok(canonical) = candidate.canonicalize() else {
        return;
    };
    if !canonical.is_dir() || is_broad_root(&canonical) {
        return;
    }
    if roots.iter().any(|root| root == &canonical) {
        return;
    }
    roots.push(canonical);
}

fn push_workspace_skill_root(roots: &mut Vec<PathBuf>, workspace_root: &Path, candidate: &Path) {
    let Ok(canonical) = candidate.canonicalize() else {
        return;
    };
    if !canonical.is_dir() || is_broad_root(&canonical) || !is_contained(workspace_root, &canonical)
    {
        return;
    }
    if roots.iter().any(|root| root == &canonical) {
        return;
    }
    roots.push(canonical);
}

fn collect_skill_entries(root: &Path, skills: &mut Vec<SkillEntry>) -> Result<()> {
    let mut candidates = vec![root.to_path_buf()];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            candidates.push(entry.path());
        }
    }
    candidates.sort();
    for dir in candidates {
        if skills.len() > MAX_SKILLS {
            break;
        }
        let skill_md = dir.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        let meta = fs::symlink_metadata(&skill_md)?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        let canonical_dir = dir
            .canonicalize()
            .with_context(|| format!("skill dir disappeared: {}", dir.display()))?;
        if !is_contained(root, &canonical_dir) {
            continue;
        }
        let (content, _) = read_bounded_text(&skill_md, MAX_INSTRUCTION_BYTES)?;
        let (name, description) = skill_metadata(&content);
        let id = skill_id(&canonical_dir);
        skills.push(SkillEntry {
            id: id.clone(),
            name: name.unwrap_or_else(|| {
                dir.file_name()
                    .and_then(|part| part.to_str())
                    .unwrap_or("skill")
                    .to_string()
            }),
            description: description.unwrap_or_default(),
            path: skill_md
                .strip_prefix(root)
                .unwrap_or(&skill_md)
                .to_string_lossy()
                .to_string(),
            entrypoint: format!("skill://{id}/SKILL.md"),
            dir: canonical_dir,
        });
    }
    Ok(())
}

fn skill_metadata(content: &str) -> (Option<String>, Option<String>) {
    if !content.starts_with("---\n") {
        return (None, None);
    }
    let Some(end) = content[4..].find("\n---") else {
        return (None, None);
    };
    let frontmatter = &content[4..4 + end];
    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().trim_matches('"').to_string());
        }
    }
    (name, description)
}

fn instruction_file_names() -> &'static [&'static str] {
    &[
        "AGENTS.md",
        "AGENTS.MD",
        "CLAUDE.md",
        "CLAUDE.MD",
        "CONTEXT.md",
        "CONTEXT.MD",
    ]
}

fn resolve_read_target(state: &AppState, ws: &Workspace, rel: &str) -> Result<ReadTarget> {
    if rel.starts_with("skill://") {
        return resolve_skill_read_target(state, ws, rel);
    }
    let absolute_path = resolve_file(ws, rel)?;
    if let Some(target) = skill_read_target_for_file(state, ws, rel, &absolute_path)? {
        return Ok(target);
    }
    Ok(ReadTarget {
        display_path: rel.to_string(),
        absolute_path,
        activate_skill_dir: None,
    })
}

fn resolve_skill_read_target(state: &AppState, ws: &Workspace, uri: &str) -> Result<ReadTarget> {
    let (skill_id, rel_path) = parse_skill_uri(uri)?;
    let (skills, _) = discover_skills(&state.config, Some(&ws.root))?;
    let Some(skill) = skills.into_iter().find(|skill| skill.id == skill_id) else {
        bail!("unknown skill id: {skill_id}");
    };
    let is_entrypoint = rel_path == "SKILL.md";
    if !is_entrypoint && !ws.activated_skill_dirs.contains(&skill.dir) {
        bail!(
            "read {} before reading other files from this skill",
            skill.entrypoint
        );
    }
    let candidate = Path::new(&rel_path);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("skill path must be relative and may not contain '..'");
    }
    let absolute_path = skill.dir.join(candidate);
    let meta = fs::symlink_metadata(&absolute_path)?;
    if meta.file_type().is_symlink() {
        bail!("path is a symlink");
    }
    if !meta.is_file() {
        bail!("not a file");
    }
    let canonical = absolute_path.canonicalize()?;
    if !is_contained(&skill.dir, &canonical) {
        bail!("skill path escapes skill root");
    }
    Ok(ReadTarget {
        display_path: format!("skill://{skill_id}/{rel_path}"),
        absolute_path: canonical,
        activate_skill_dir: is_entrypoint.then_some(skill.dir),
    })
}

fn skill_read_target_for_file(
    state: &AppState,
    ws: &Workspace,
    rel: &str,
    absolute_path: &Path,
) -> Result<Option<ReadTarget>> {
    let (skills, _) = discover_skills(&state.config, Some(&ws.root))?;
    let Some(skill) = skills
        .into_iter()
        .filter(|skill| is_contained(&skill.dir, absolute_path))
        .max_by_key(|skill| skill.dir.components().count())
    else {
        return Ok(None);
    };
    let skill_file = skill.dir.join("SKILL.md");
    let is_entrypoint = skill_file
        .canonicalize()
        .map(|path| path == absolute_path)
        .unwrap_or(false);
    if !is_entrypoint && !ws.activated_skill_dirs.contains(&skill.dir) {
        bail!(
            "read {} before reading other files from this skill",
            skill.entrypoint
        );
    }
    Ok(Some(ReadTarget {
        display_path: rel.to_string(),
        absolute_path: absolute_path.to_path_buf(),
        activate_skill_dir: is_entrypoint.then_some(skill.dir),
    }))
}

fn parse_skill_uri(uri: &str) -> Result<(String, String)> {
    let Some(rest) = uri.strip_prefix("skill://") else {
        bail!("skill uri must start with skill://");
    };
    let Some((skill_id, rel_path)) = rest.split_once('/') else {
        bail!("skill uri must include a skill id and path");
    };
    if skill_id.is_empty() || rel_path.is_empty() {
        bail!("skill uri must include a skill id and path");
    }
    Ok((skill_id.to_string(), rel_path.to_string()))
}

fn activate_skill_dir(state: &AppState, workspace_id: &str, skill_dir: PathBuf) {
    let mut registry = state.registry.lock().unwrap();
    if let Some(workspace) = registry.workspaces.get_mut(workspace_id) {
        workspace.activated_skill_dirs.insert(skill_dir);
    }
}

fn skill_id(dir: &Path) -> String {
    let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
    digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn required<'a>(args: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required argument: {key}"))
}

fn optional_bool(args: &serde_json::Map<String, Value>, key: &str) -> Result<Option<bool>> {
    match args.get(key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| anyhow!("{key} must be a boolean")),
        None => Ok(None),
    }
}

fn optional_u64(args: &serde_json::Map<String, Value>, key: &str) -> Result<Option<u64>> {
    match args.get(key) {
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow!("{key} must be a non-negative integer")),
        None => Ok(None),
    }
}

fn optional_short_text(
    args: &serde_json::Map<String, Value>,
    key: &str,
    limit: usize,
) -> Result<Option<String>> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(text) = value.as_str() else {
        bail!("{key} must be a string");
    };
    ensure_short_text(key, text, limit)?;
    Ok(Some(text.to_string()))
}

fn ensure_short_text(key: &str, text: &str, limit: usize) -> Result<()> {
    ensure_text_content(text)?;
    if text.len() > limit {
        bail!("{key} exceeds maximum length");
    }
    Ok(())
}

fn ensure_review_severity(value: &str) -> Result<()> {
    ensure_short_text("severity", value, 20)?;
    if !matches!(value, "info" | "low" | "medium" | "high") {
        bail!("severity must be one of info, low, medium, or high");
    }
    Ok(())
}

fn ensure_workspace_relative_path_arg(value: &str) -> Result<()> {
    ensure_short_text("path", value, 240)?;
    let candidate = Path::new(value);
    if value.is_empty() || candidate.is_absolute() {
        bail!("path must be workspace-relative");
    }
    if candidate
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("path may not contain '..': {value}");
    }
    Ok(())
}

fn ensure_text_content(text: &str) -> Result<()> {
    if text.contains('\0') {
        bail!("content contains NUL bytes and is treated as binary");
    }
    Ok(())
}

fn resolve_file(ws: &Workspace, rel: &str) -> Result<PathBuf> {
    let path = resolve_path(ws, rel)?;
    let meta = fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() {
        bail!("path is a symlink");
    }
    if !meta.is_file() {
        bail!("not a file");
    }
    Ok(path)
}

fn resolve_dir(ws: &Workspace, rel: &str) -> Result<PathBuf> {
    let path = resolve_path(ws, rel)?;
    let meta = fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() {
        bail!("path is a symlink");
    }
    if !meta.is_dir() {
        bail!("not a directory");
    }
    Ok(path)
}

fn resolve_write_path(ws: &Workspace, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel == "." {
        bail!("path must be a workspace-relative file path");
    }
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        bail!("path must be workspace-relative: {rel}");
    }
    if candidate
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("path may not contain '..': {rel}");
    }
    let Some(file_name) = candidate.file_name() else {
        bail!("path must include a file name");
    };
    let parent = candidate.parent().unwrap_or_else(|| Path::new("."));
    let parent_path = if parent.as_os_str().is_empty() || parent == Path::new(".") {
        ws.root.clone()
    } else {
        resolve_dir(ws, &parent.to_string_lossy())?
    };
    let target = parent_path.join(file_name);
    if fs::symlink_metadata(&target)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        bail!("path is a symlink");
    }
    if fs::metadata(&target)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
    {
        bail!("path is a directory");
    }
    if !is_contained(&ws.root, &parent_path) || !target.starts_with(&ws.root) {
        bail!("path escapes workspace root");
    }
    Ok(target)
}

fn resolve_path(ws: &Workspace, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel == "." {
        return Ok(ws.root.clone());
    }
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        bail!("path must be workspace-relative: {rel}");
    }
    if candidate
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("path may not contain '..': {rel}");
    }
    let joined = ws.root.join(candidate);
    if fs::symlink_metadata(&joined)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        bail!("path is a symlink");
    }
    let canonical = joined.canonicalize()?;
    if !is_contained(&ws.root, &canonical) {
        bail!("path escapes workspace root");
    }
    Ok(canonical)
}

fn contains(ws: &Workspace, path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => return false,
        Ok(_) => {}
        Err(_) => return false,
    }
    path.canonicalize()
        .map(|canonical| is_contained(&ws.root, &canonical))
        .unwrap_or(false)
}

fn is_contained(root: &Path, target: &Path) -> bool {
    target == root || target.starts_with(root)
}

fn is_ignored_entry(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|name| IGNORED_DIRS.contains(&name))
        .unwrap_or(false)
}

fn expand_home(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

fn is_broad_root(path: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| path == home.canonicalize().unwrap_or(home))
        .unwrap_or(false)
}

fn absolutize(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(env::current_dir()?.join(path))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    git_limited(root, args, MAX_READ_BYTES * 4)
}

fn git_limited(root: &Path, args: &[&str], limit: usize) -> Result<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| "git is not installed")?;
    let Some(stdout) = child.stdout.take() else {
        bail!("git stdout unavailable");
    };
    let reader = thread::spawn(move || {
        let mut stdout = stdout;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = stdout.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
            if remaining > 0 {
                bytes.extend_from_slice(&buffer[..read.min(remaining)]);
            }
        }
        Ok::<Vec<u8>, std::io::Error>(bytes)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > GIT_TIMEOUT {
            let _ = child.kill();
            bail!("git command timed out");
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = reader
        .join()
        .map_err(|_| anyhow!("git stdout reader panicked"))??;
    if !status.success() {
        bail!("git command failed");
    }
    Ok(String::from_utf8_lossy(&stdout).to_string())
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn gh_available() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn git_worktree_available(roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| {
        Command::new("git")
            .args(["worktree", "list"])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

fn git_push_with_output(root: &Path, remote: &str, refspec: &str) -> Result<GitCommandOutput> {
    command_with_output(
        "git",
        &["push", "-u", remote, refspec],
        root,
        GIT_PUSH_TIMEOUT,
    )
}

fn command_with_output<S: AsRef<str>>(
    program: &str,
    args: &[S],
    cwd: &Path,
    timeout: Duration,
) -> Result<GitCommandOutput> {
    let mut child = Command::new(program)
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("{program} is not installed"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("{program} stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("{program} stderr unavailable"))?;
    let stdout_reader = thread::spawn(move || read_pipe_limited(stdout, MAX_SHELL_OUTPUT_BYTES));
    let stderr_reader = thread::spawn(move || read_pipe_limited(stderr, MAX_SHELL_OUTPUT_BYTES));
    let start = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let status = child.wait().ok();
            break (status, true);
        }
        thread::sleep(Duration::from_millis(25));
    };
    let mut stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("{program} stdout reader panicked"))??;
    let mut stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("{program} stderr reader panicked"))??;
    let stdout_truncated = stdout.len() > MAX_SHELL_OUTPUT_BYTES;
    let stderr_truncated = stderr.len() > MAX_SHELL_OUTPUT_BYTES;
    stdout.truncate(MAX_SHELL_OUTPUT_BYTES);
    stderr.truncate(MAX_SHELL_OUTPUT_BYTES);
    Ok(GitCommandOutput {
        exit_code: status.and_then(|status| status.code()),
        stdout,
        stderr,
        timed_out,
        truncated: stdout_truncated || stderr_truncated,
    })
}

fn worktrees_root(state_dir: &Path) -> PathBuf {
    state_dir.join("worktrees")
}

fn worktree_metadata_root(state_dir: &Path) -> PathBuf {
    state_dir.join("worktree-metadata")
}

fn pr_bodies_root(state_dir: &Path) -> PathBuf {
    state_dir.join("pr-bodies")
}

fn write_pr_body_file(state_dir: &Path, body: &str) -> Result<PathBuf> {
    let dir = pr_bodies_root(state_dir);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", Uuid::new_v4().simple()));
    fs::write(&path, body)?;
    Ok(path)
}

fn worktree_metadata_path(state_dir: &Path, managed_name: &str) -> PathBuf {
    worktree_metadata_root(state_dir).join(format!("{managed_name}.json"))
}

fn write_worktree_metadata(
    state_dir: &Path,
    managed_name: &str,
    metadata: &ManagedWorktreeMetadata,
) -> Result<()> {
    let dir = worktree_metadata_root(state_dir);
    fs::create_dir_all(&dir)?;
    fs::write(
        worktree_metadata_path(state_dir, managed_name),
        serde_json::to_vec_pretty(metadata)?,
    )?;
    Ok(())
}

fn read_worktree_metadata(state_dir: &Path, managed_name: &str) -> Result<ManagedWorktreeMetadata> {
    let path = worktree_metadata_path(state_dir, managed_name);
    if !path.exists() {
        return Ok(ManagedWorktreeMetadata::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn managed_worktrees(state_dir: &Path) -> Result<Vec<PathBuf>> {
    let root = worktrees_root(state_dir);
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut worktrees = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let meta = entry.file_type()?;
        if meta.is_dir() {
            worktrees.push(entry.path());
        }
    }
    worktrees.sort();
    Ok(worktrees)
}

fn remove_managed_worktree(path: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => {
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
            Ok(())
        }
    }
}

fn validate_git_ref_arg(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("git ref must be non-empty");
    }
    ensure_text_content(value)?;
    if value.starts_with('-') {
        bail!("git ref may not start with '-'");
    }
    Ok(())
}

fn validate_git_remote_arg(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("git remote must be non-empty");
    }
    ensure_text_content(value)?;
    if value.starts_with('-') {
        bail!("git remote may not start with '-'");
    }
    if value.len() > 120
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        bail!("git remote contains unsupported characters");
    }
    Ok(())
}

fn validate_branch_name(root: &Path, branch: &str) -> Result<()> {
    validate_git_ref_arg(branch)?;
    let status = Command::new("git")
        .args(["check-ref-format", "--branch", branch])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| "git is not installed")?;
    if !status.success() {
        bail!("invalid branch name");
    }
    Ok(())
}

fn scrub_git_output(value: &str) -> String {
    value
        .split_whitespace()
        .map(|part| {
            if let Some((scheme, rest)) = part.split_once("://") {
                if let Some((_, host_path)) = rest.split_once('@') {
                    return format!("{scheme}://<redacted>@{host_path}");
                }
            }
            part.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_first_url(value: &str) -> Option<String> {
    value
        .split_whitespace()
        .find(|part| part.starts_with("https://") || part.starts_with("http://"))
        .map(|part| {
            part.trim_matches(|ch: char| matches!(ch, ',' | ')' | ']' | '"' | '\''))
                .to_string()
        })
}

fn validate_base_ref(root: &Path, base_ref: &str) -> Result<()> {
    let rev = format!("{base_ref}^{{commit}}");
    let status = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(rev)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| "git is not installed")?;
    if !status.success() {
        bail!("base_ref does not resolve to a commit");
    }
    Ok(())
}

fn bash_available() -> bool {
    Command::new("/bin/bash")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn read_pipe_limited<R: Read>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
        if remaining > 0 {
            bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(bytes)
}

fn safe_shell_env() -> Vec<(String, String)> {
    let allowed = ["PATH", "HOME", "TMPDIR", "USER", "SHELL", "TERM"];
    allowed
        .into_iter()
        .filter_map(|key| {
            if is_secret_env_name(key) {
                return None;
            }
            env::var(key).ok().map(|value| (key.to_string(), value))
        })
        .collect()
}

fn is_secret_env_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "credential",
        "api_key",
        "apikey",
        "private_key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn register_session(
    mut sessions: std::sync::MutexGuard<'_, InitializedSessions>,
    session_id: String,
) {
    if sessions.set.insert(session_id.clone()) {
        sessions.order.push_back(session_id);
    }
    while sessions.order.len() > MAX_INITIALIZED_SESSIONS {
        if let Some(oldest) = sessions.order.pop_front() {
            sessions.set.remove(&oldest);
        }
    }
}

fn session_initialized(state: &AppState, session_id: Option<&str>) -> bool {
    let Some(session_id) = session_id else {
        return false;
    };
    state
        .initialized_sessions
        .lock()
        .unwrap()
        .set
        .contains(session_id)
}

fn tool_result(tool: &str, payload: Value, is_error: bool) -> Value {
    let text = payload.to_string();
    let meta = tool_result_meta(tool, &payload, is_error);
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
        "structuredContent": payload,
        "_meta": meta
    })
}

fn tool_result_meta(tool: &str, payload: &Value, is_error: bool) -> Value {
    let summary = if is_error {
        json!({
            "error_chars": payload.get("error").and_then(Value::as_str).map(str::len).unwrap_or(0)
        })
    } else {
        summarize_result(tool, payload)
    };
    let mut meta = json!({
        "codex-web-bridge/tool": tool,
        "codex-web-bridge/is_error": is_error,
        "codex-web-bridge/summary": summary
    });
    let output_template = match tool {
        "render_changes" if !is_error => Some(CHANGES_WIDGET_URI),
        "render_review" if !is_error => Some(REVIEW_WIDGET_URI),
        "render_pull_requests" if !is_error => Some(PULL_REQUESTS_WIDGET_URI),
        "render_edit_plans" if !is_error => Some(EDIT_PLANS_WIDGET_URI),
        _ => None,
    };
    if let Some(resource_uri) = output_template {
        if let Some(map) = meta.as_object_mut() {
            map.insert("openai/outputTemplate".to_string(), json!(resource_uri));
            map.insert("ui".to_string(), json!({"resourceUri": resource_uri}));
        }
    }
    meta
}

fn tool_definitions(trust_level: TrustLevel) -> Vec<Value> {
    tool_names(trust_level)
        .into_iter()
        .filter_map(tool_definition)
        .collect()
}

fn tool_names(trust_level: TrustLevel) -> Vec<&'static str> {
    let mut tools = vec![
        "open_workspace",
        "read",
        "list",
        "search",
        "git_status",
        "git_diff",
        "preview_patch",
        "show_session",
        "show_changes",
        "render_changes",
        "show_review",
        "render_review",
        "list_worktrees",
        "list_pull_requests",
        "show_pull_requests",
        "render_pull_requests",
        "list_notes",
        "list_edit_plans",
        "show_edit_plans",
        "render_edit_plans",
        "list_skills",
    ];
    if matches!(trust_level, TrustLevel::Review | TrustLevel::Execute) {
        tools.push("create_note");
        tools.push("create_edit_plan");
        tools.push("update_edit_plan_status");
    }
    if trust_level == TrustLevel::Execute {
        tools.extend([
            "write",
            "edit",
            "apply_patch",
            "move_path",
            "shell",
            "open_worktree",
            "publish_branch",
            "create_pull_request",
            "refresh_pull_request_status",
            "refresh_pull_requests",
        ]);
    }
    tools
}

fn tool_definition(name: &str) -> Option<Value> {
    Some(match name {
        "open_workspace" =>
        tool_def(
            "open_workspace",
            "Open Workspace",
            "Open a path inside an allowed root and return a workspace id, root project instructions, nested instruction file paths, and configured or workspace-local skill entrypoints.",
            json!({"type":"object","properties":{"path":{"type":"string","description":"Absolute path inside an allowed root."}},"required":["path"]}),
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"name":{"type":"string"},"instructions":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"truncated":{"type":"boolean"}},"required":["path","content","truncated"],"additionalProperties":false}},"available_instructions":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}},"available_instructions_truncated":{"type":"boolean"},"skills":{"type":"array","items":{"type":"object","properties":{"skill_id":{"type":"string"},"name":{"type":"string"},"description":{"type":"string"},"path":{"type":"string"},"entrypoint":{"type":"string"}},"required":["skill_id","name","description","path","entrypoint"],"additionalProperties":false}},"skills_truncated":{"type":"boolean"}},"required":["workspace_id","name","instructions","available_instructions","available_instructions_truncated","skills","skills_truncated"],"additionalProperties":false}),
        ),
        "read" =>
        tool_def(
            "read",
            "Read",
            "Read bounded text from a workspace-relative file, or from a configured skill:// entrypoint/resource. Skill resources other than SKILL.md require reading that skill's SKILL.md first.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"path":{"type":"string","description":"Workspace-relative file path, or skill://<skill_id>/SKILL.md / skill://<skill_id>/<resource>."}},"required":["workspace_id","path"]}),
            json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"truncated":{"type":"boolean"}},"required":["path","content","truncated"],"additionalProperties":false}),
        ),
        "list" =>
        tool_def(
            "list",
            "List",
            "List a workspace-relative directory with bounded entries.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"path":{"type":"string","description":"Workspace-relative directory (default '.')."}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"entries":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"dir":{"type":"boolean"},"symlink":{"type":"boolean"}},"required":["name","dir","symlink"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["entries","truncated"],"additionalProperties":false}),
        ),
        "search" =>
        tool_def(
            "search",
            "Search",
            "Search text with ignore rules and bounded results.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"query":{"type":"string"},"path":{"type":"string","description":"Workspace-relative base directory (default '.')."}},"required":["workspace_id","query"]}),
            json!({"type":"object","properties":{"results":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"line":{"type":"integer"},"text":{"type":"string"}},"required":["path","line","text"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["results","truncated"],"additionalProperties":false}),
        ),
        "git_status" =>
        tool_def(
            "git_status",
            "Git Status",
            "Return branch, HEAD, and short status.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"branch":{"type":"string"},"head":{"type":"string"},"status":{"type":"string"}},"required":["branch","head","status"],"additionalProperties":false}),
        ),
        "git_diff" =>
        tool_def(
            "git_diff",
            "Git Diff",
            "Return bounded diff and stat for the workspace.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"stat":{"type":"string"},"diff":{"type":"string"},"truncated":{"type":"boolean"}},"required":["stat","diff","truncated"],"additionalProperties":false}),
        ),
        "preview_patch" =>
        tool_def(
            "preview_patch",
            "Preview Patch",
            "Validate a bounded unified diff against the workspace and return the files and diff that would change without writing files.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"patch":{"type":"string","description":"Unified diff with ---/+++ file headers and @@ hunks."}},"required":["workspace_id","patch"]}),
            json!({"type":"object","properties":{"would_apply":{"type":"boolean"},"files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}},"diff":{"type":"string"},"truncated":{"type":"boolean"}},"required":["would_apply","files","diff","truncated"],"additionalProperties":false}),
        ),
        "show_session" =>
        tool_def(
            "show_session",
            "Show Session",
            "Show recent audited tool calls for the current or requested session without file contents.",
            json!({"type":"object","properties":{"session_id":{"type":"string","description":"MCP session id. Defaults to the current session."},"limit":{"type":"integer","description":"Maximum audit events to inspect (default 100)."}}}),
            json!({"type":"object","properties":{"session_id":{"type":"string"},"call_count":{"type":"integer"},"calls":{"type":"array","items":{"type":"object"}}},"required":["session_id","call_count","calls"],"additionalProperties":false}),
        ),
        "show_changes" =>
        tool_def(
            "show_changes",
            "Show Changes",
            "Summarize current Git changes and recent change-oriented tool actions for a workspace.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"branch":{"type":"string"},"head":{"type":"string"},"status":{"type":"string"},"stat":{"type":"string"},"diff":{"type":"string"},"truncated":{"type":"boolean"},"recent_actions":{"type":"array","items":{"type":"object"}}},"required":["workspace_id","branch","head","status","stat","diff","truncated","recent_actions"],"additionalProperties":false}),
        ),
        "render_changes" =>
        apps_widget_tool_def(
            "render_changes",
            "Render Changes",
            "Render current Git changes and recent change-oriented tool actions using the bundled Apps widget.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"branch":{"type":"string"},"head":{"type":"string"},"status":{"type":"string"},"stat":{"type":"string"},"diff":{"type":"string"},"truncated":{"type":"boolean"},"recent_actions":{"type":"array","items":{"type":"object"}}},"required":["workspace_id","branch","head","status","stat","diff","truncated","recent_actions"],"additionalProperties":false}),
        ),
        "show_review" =>
        tool_def(
            "show_review",
            "Show Review",
            "Summarize recoverable review notes and edit plans for a workspace. Requires authenticated connector access because note bodies are returned.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"severity":{"type":"string","enum":["info","low","medium","high"],"description":"Optional note severity filter."},"path":{"type":"string","description":"Optional note path filter."},"limit":{"type":"integer","description":"Maximum notes and plans to return (default 25)."}},"required":["workspace_id"]}),
            review_handoff_output_schema(),
        ),
        "render_review" =>
        apps_widget_tool_def(
            "render_review",
            "Render Review",
            "Render recoverable review notes and edit plans using the bundled Apps widget. Requires authenticated connector access because note bodies are returned.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"severity":{"type":"string","enum":["info","low","medium","high"],"description":"Optional note severity filter."},"path":{"type":"string","description":"Optional note path filter."},"limit":{"type":"integer","description":"Maximum notes and plans to return (default 25)."}},"required":["workspace_id"]}),
            review_handoff_output_schema(),
        ),
        "list_worktrees" =>
        tool_def(
            "list_worktrees",
            "List Worktrees",
            "List managed Git worktrees and return workspace ids that can be used with read, show_changes, and other workspace tools.",
            json!({"type":"object","properties":{}}),
            json!({"type":"object","properties":{"worktrees":{"type":"array","items":{"type":"object","properties":{"workspace_id":{"type":"string"},"name":{"type":"string"},"managed_name":{"type":"string"},"branch":{"type":"string"},"head":{"type":"string"},"status":{"type":"string"},"task_id":{"type":["string","null"]},"task":{"type":["string","null"]},"available":{"type":"boolean"},"error":{"type":"string"}},"required":["managed_name","available"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["worktrees","truncated"],"additionalProperties":false}),
        ),
        "list_pull_requests" =>
        tool_def(
            "list_pull_requests",
            "List Pull Requests",
            "List pull request handoff records created through this connector without reading PR bodies.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string","description":"Optional workspace id filter."},"branch":{"type":"string","description":"Optional branch filter."},"limit":{"type":"integer","description":"Maximum records to return (default 50)."}}}),
            json!({"type":"object","properties":{"pull_requests":{"type":"array","items":{"type":"object","properties":{"created_unix_ms":{"type":"integer"},"updated_unix_ms":{"type":"integer"},"session_id":{"type":"string"},"workspace_id":{"type":"string"},"branch":{"type":"string"},"base":{"type":"string"},"title":{"type":"string"},"draft":{"type":"boolean"},"status":{"type":"string"},"url":{"type":"string"},"number":{"type":"integer"},"remote_state":{"type":"string"},"merged":{"type":"boolean"},"exit_code":{"type":"integer"},"body_chars":{"type":"integer"}},"required":["created_unix_ms","updated_unix_ms","workspace_id","branch","title","draft","status","body_chars"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["pull_requests","truncated"],"additionalProperties":false}),
        ),
        "show_pull_requests" =>
        tool_def(
            "show_pull_requests",
            "Show Pull Requests",
            "Summarize connector-created pull request handoff records and lifecycle status counts without reading PR bodies or calling GitHub.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string","description":"Optional workspace id filter."},"branch":{"type":"string","description":"Optional branch filter."},"limit":{"type":"integer","description":"Maximum records to return (default 50)."}}}),
            pull_requests_handoff_output_schema(),
        ),
        "render_pull_requests" =>
        apps_widget_tool_def(
            "render_pull_requests",
            "Render Pull Requests",
            "Render connector-created pull request handoff records using the bundled Apps widget without reading PR bodies or calling GitHub.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string","description":"Optional workspace id filter."},"branch":{"type":"string","description":"Optional branch filter."},"limit":{"type":"integer","description":"Maximum records to return (default 50)."}}}),
            pull_requests_handoff_output_schema(),
        ),
        "list_notes" =>
        tool_def(
            "list_notes",
            "List Notes",
            "List persisted review notes created through this connector so later agents can recover prior findings.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string","description":"Workspace id whose notes should be listed."},"severity":{"type":"string","enum":["info","low","medium","high"],"description":"Optional severity filter."},"path":{"type":"string","description":"Optional workspace-relative path filter."},"limit":{"type":"integer","description":"Maximum records to return (default 50)."}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"notes":{"type":"array","items":{"type":"object","properties":{"note_id":{"type":"string"},"ts_unix_ms":{"type":"integer"},"session_id":{"type":["string","null"]},"workspace_id":{"type":"string"},"title":{"type":"string"},"severity":{"type":"string"},"path":{"type":["string","null"]},"body":{"type":"string"}},"required":["note_id","ts_unix_ms","workspace_id","title","severity","path","body"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["notes","truncated"],"additionalProperties":false}),
        ),
        "list_edit_plans" =>
        tool_def(
            "list_edit_plans",
            "List Edit Plans",
            "List persisted edit plans created through this connector without reading file contents. Requires authenticated connector access because plan intent is returned.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string","description":"Optional workspace id filter."},"status":{"type":"string","enum":["draft","approved","superseded","applied"],"description":"Optional edit plan lifecycle status filter."},"limit":{"type":"integer","description":"Maximum records to return (default 50)."}}}),
            json!({"type":"object","properties":{"edit_plans":{"type":"array","items":{"type":"object","properties":{"created_unix_ms":{"type":"integer"},"updated_unix_ms":{"type":"integer"},"session_id":{"type":"string"},"workspace_id":{"type":"string"},"plan_id":{"type":"string"},"status":{"type":"string"},"title":{"type":"string"},"intent":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}},"patch_chars":{"type":"integer"},"files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}},"status_note":{"type":"string"},"approved_unix_ms":{"type":"integer"},"applied_unix_ms":{"type":"integer"},"applied_session_id":{"type":"string"},"applied_files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}}},"required":["created_unix_ms","updated_unix_ms","workspace_id","plan_id","status","title","intent","paths","files","applied_files"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["edit_plans","truncated"],"additionalProperties":false}),
        ),
        "show_edit_plans" =>
        tool_def(
            "show_edit_plans",
            "Show Edit Plans",
            "Summarize persisted edit plan history and lifecycle status counts. Requires authenticated connector access because plan intent is returned.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string","description":"Optional workspace id filter."},"status":{"type":"string","enum":["draft","approved","superseded","applied"],"description":"Optional edit plan lifecycle status filter."},"limit":{"type":"integer","description":"Maximum records to return (default 50)."}}}),
            edit_plans_handoff_output_schema(),
        ),
        "render_edit_plans" =>
        apps_widget_tool_def(
            "render_edit_plans",
            "Render Edit Plans",
            "Render persisted edit plan history and lifecycle status counts using the bundled Apps widget. Requires authenticated connector access because plan intent is returned.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string","description":"Optional workspace id filter."},"status":{"type":"string","enum":["draft","approved","superseded","applied"],"description":"Optional edit plan lifecycle status filter."},"limit":{"type":"integer","description":"Maximum records to return (default 50)."}}}),
            edit_plans_handoff_output_schema(),
        ),
        "publish_branch" =>
        mutating_tool_def(
            "publish_branch",
            "Publish Branch",
            "Push the current workspace branch to a Git remote and set upstream. Execute trust is required.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"remote":{"type":"string","description":"Git remote name (default origin)."},"remote_branch":{"type":"string","description":"Remote branch name (default current branch)."}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"branch":{"type":"string"},"remote":{"type":"string"},"remote_branch":{"type":"string"},"exit_code":{"type":["integer","null"]},"success":{"type":"boolean"},"timed_out":{"type":"boolean"},"stdout":{"type":"string"},"stderr":{"type":"string"},"truncated":{"type":"boolean"}},"required":["branch","remote","remote_branch","exit_code","success","timed_out","stdout","stderr","truncated"],"additionalProperties":false}),
        ),
        "create_pull_request" =>
        network_tool_def(
            "create_pull_request",
            "Create Pull Request",
            "Create a GitHub pull request for the current workspace branch with gh pr create. Execute trust is required.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"base":{"type":"string","description":"Optional base branch. Defaults to gh/repository default behavior."},"draft":{"type":"boolean","description":"Create a draft pull request."}},"required":["workspace_id","title","body"]}),
            json!({"type":"object","properties":{"branch":{"type":"string"},"base":{"type":["string","null"]},"title":{"type":"string"},"draft":{"type":"boolean"},"exit_code":{"type":["integer","null"]},"success":{"type":"boolean"},"timed_out":{"type":"boolean"},"url":{"type":["string","null"]},"body_chars":{"type":"integer"},"stdout":{"type":"string"},"stderr":{"type":"string"},"truncated":{"type":"boolean"}},"required":["branch","base","title","draft","exit_code","success","timed_out","url","body_chars","stdout","stderr","truncated"],"additionalProperties":false}),
        ),
        "refresh_pull_request_status" =>
        network_tool_def(
            "refresh_pull_request_status",
            "Refresh Pull Request Status",
            "Refresh a persisted pull request handoff record from GitHub with gh pr view. Updates connector state only; workspace files are not changed. Execute trust is required.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"branch":{"type":"string","description":"Optional PR head branch. Defaults to the current workspace branch."},"url":{"type":"string","description":"Optional GitHub PR URL selector. If provided, it is used instead of branch."}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"success":{"type":"boolean"},"selector":{"type":"string"},"exit_code":{"type":["integer","null"]},"timed_out":{"type":"boolean"},"stdout":{"type":"string"},"stderr":{"type":"string"},"truncated":{"type":"boolean"},"pull_request":{"type":["object","null"],"properties":{"created_unix_ms":{"type":"integer"},"updated_unix_ms":{"type":"integer"},"session_id":{"type":"string"},"workspace_id":{"type":"string"},"branch":{"type":"string"},"base":{"type":"string"},"title":{"type":"string"},"draft":{"type":"boolean"},"status":{"type":"string"},"url":{"type":"string"},"number":{"type":"integer"},"remote_state":{"type":"string"},"merged":{"type":"boolean"},"exit_code":{"type":"integer"},"body_chars":{"type":"integer"}},"required":["created_unix_ms","updated_unix_ms","workspace_id","branch","title","draft","status","body_chars"],"additionalProperties":false}},"required":["success","selector","exit_code","timed_out","stdout","stderr","truncated","pull_request"],"additionalProperties":false}),
        ),
        "refresh_pull_requests" =>
        network_tool_def(
            "refresh_pull_requests",
            "Refresh Pull Requests",
            "Refresh multiple persisted pull request handoff records for an opened workspace from GitHub with gh pr view. Uses each record's PR URL when available, falls back to branch, and updates connector state only. Execute trust is required.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"branch":{"type":"string","description":"Optional PR head branch filter."},"limit":{"type":"integer","description":"Maximum records to refresh (default 5, capped at 5)."}},"required":["workspace_id"]}),
            refresh_pull_requests_output_schema(),
        ),
        "list_skills" =>
        tool_def(
            "list_skills",
            "List Skills",
            "List configured agent skills and their skill:// SKILL.md entrypoints. Hosts must read a skill's SKILL.md before using files from that skill directory.",
            json!({"type":"object","properties":{}}),
            json!({"type":"object","properties":{"skills":{"type":"array","items":{"type":"object","properties":{"skill_id":{"type":"string"},"name":{"type":"string"},"description":{"type":"string"},"path":{"type":"string"},"entrypoint":{"type":"string"}},"required":["skill_id","name","description","path","entrypoint"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["skills","truncated"],"additionalProperties":false}),
        ),
        "create_note" =>
        review_tool_def(
            "create_note",
            "Create Note",
            "Save a structured review note under connector state without mutating the workspace. Requires trust_level=review or trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"severity":{"type":"string","enum":["info","low","medium","high"],"description":"Review severity (default info)."},"path":{"type":"string","description":"Optional workspace-relative file path the note refers to."}},"required":["workspace_id","title","body"]}),
            json!({"type":"object","properties":{"note_id":{"type":"string"},"workspace_id":{"type":"string"},"title":{"type":"string"},"severity":{"type":"string"},"path":{"type":["string","null"]},"body_chars":{"type":"integer"}},"required":["note_id","workspace_id","title","severity","path","body_chars"],"additionalProperties":false}),
        ),
        "create_edit_plan" =>
        review_tool_def(
            "create_edit_plan",
            "Create Edit Plan",
            "Save a structured edit plan under connector state without mutating workspace files. Requires trust_level=review or trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"title":{"type":"string"},"intent":{"type":"string"},"paths":{"type":"array","items":{"type":"string"},"description":"Workspace-relative files expected to change."},"patch":{"type":"string","description":"Optional unified diff to validate and summarize without storing diff content."}},"required":["workspace_id","title","intent","paths"]}),
            json!({"type":"object","properties":{"plan_id":{"type":"string"},"workspace_id":{"type":"string"},"status":{"type":"string"},"title":{"type":"string"},"intent_chars":{"type":"integer"},"paths":{"type":"array","items":{"type":"string"}},"patch_chars":{"type":["integer","null"]},"files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}}},"required":["plan_id","workspace_id","status","title","intent_chars","paths","patch_chars","files"],"additionalProperties":false}),
        ),
        "update_edit_plan_status" =>
        review_tool_def(
            "update_edit_plan_status",
            "Update Edit Plan Status",
            "Update an edit plan lifecycle status under connector state without mutating workspace files. Use apply_patch with plan_id to mark a plan applied.",
            json!({"type":"object","properties":{"plan_id":{"type":"string"},"status":{"type":"string","enum":["draft","approved","superseded"]},"status_note":{"type":"string","description":"Optional short review note for this status change."}},"required":["plan_id","status"]}),
            json!({"type":"object","properties":{"plan_id":{"type":"string"},"workspace_id":{"type":"string"},"status":{"type":"string"},"title":{"type":"string"},"status_note":{"type":["string","null"]},"approved_unix_ms":{"type":["integer","null"]},"updated_unix_ms":{"type":"integer"}},"required":["plan_id","workspace_id","status","title","status_note","approved_unix_ms","updated_unix_ms"],"additionalProperties":false}),
        ),
        "write" =>
        mutating_tool_def(
            "write",
            "Write",
            "Create or overwrite a workspace-relative UTF-8 text file. Requires trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"path":{"type":"string"},"content":{"type":"string"}},"required":["workspace_id","path","content"]}),
            json!({"type":"object","properties":{"path":{"type":"string"},"created":{"type":"boolean"},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"},"diff":{"type":"string"},"truncated":{"type":"boolean"}},"required":["path","created","bytes_before","bytes_after","diff","truncated"],"additionalProperties":false}),
        ),
        "edit" =>
        mutating_tool_def(
            "edit",
            "Edit",
            "Apply an exact-match UTF-8 text replacement to a workspace-relative file. Requires trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"replace_all":{"type":"boolean","description":"Replace all matches instead of requiring one match."},"expected_replacements":{"type":"integer","description":"Optional exact replacement count guard."}},"required":["workspace_id","path","old_text","new_text"]}),
            json!({"type":"object","properties":{"path":{"type":"string"},"replacements":{"type":"integer"},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"},"diff":{"type":"string"},"truncated":{"type":"boolean"}},"required":["path","replacements","bytes_before","bytes_after","diff","truncated"],"additionalProperties":false}),
        ),
        "apply_patch" =>
        mutating_tool_def(
            "apply_patch",
            "Apply Patch",
            "Apply a bounded unified diff patch to UTF-8 files under the workspace, including add/delete file hunks. Requires trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"patch":{"type":"string","description":"Unified diff with ---/+++ file headers and @@ hunks."},"plan_id":{"type":"string","description":"Optional approved edit plan id to mark applied after a successful patch."}},"required":["workspace_id","patch"]}),
            json!({"type":"object","properties":{"files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}},"diff":{"type":"string"},"truncated":{"type":"boolean"},"plan_id":{"type":"string"},"plan_status":{"type":"string"}},"required":["files","diff","truncated"],"additionalProperties":false}),
        ),
        "move_path" =>
        mutating_tool_def(
            "move_path",
            "Move Path",
            "Move or rename a regular file within the workspace. Requires trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"from_path":{"type":"string"},"to_path":{"type":"string"},"overwrite":{"type":"boolean","description":"Replace an existing regular destination file. Defaults to false."}},"required":["workspace_id","from_path","to_path"]}),
            json!({"type":"object","properties":{"from_path":{"type":"string"},"to_path":{"type":"string"},"overwritten":{"type":"boolean"},"bytes":{"type":"integer"}},"required":["from_path","to_path","overwritten","bytes"],"additionalProperties":false}),
        ),
        "shell" =>
        mutating_tool_def(
            "shell",
            "Shell",
            "Run a non-interactive /bin/bash command in a workspace-contained cwd with scrubbed environment, timeout, and bounded output. Requires trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"command":{"type":"string"},"cwd":{"type":"string","description":"Workspace-relative directory (default '.')."},"timeout_ms":{"type":"integer","description":"Requested timeout in milliseconds, capped by the server."}},"required":["workspace_id","command"]}),
            json!({"type":"object","properties":{"cwd":{"type":"string"},"exit_code":{"type":["integer","null"]},"timed_out":{"type":"boolean"},"stdout":{"type":"string"},"stderr":{"type":"string"},"truncated":{"type":"boolean"}},"required":["cwd","exit_code","timed_out","stdout","stderr","truncated"],"additionalProperties":false}),
        ),
        "open_worktree" =>
        mutating_tool_def(
            "open_worktree",
            "Open Worktree",
            "Create a managed Git worktree under connector state and return a workspace id for it. Requires trust_level=execute.",
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"base_ref":{"type":"string","description":"Git commit-ish base ref (default HEAD)."},"branch":{"type":"string","description":"Optional branch name. Defaults to a codex/worktree-* branch."},"task_id":{"type":"string","description":"Optional external task or issue id for this worktree."},"task":{"type":"string","description":"Optional short task description for this worktree."}},"required":["workspace_id"]}),
            json!({"type":"object","properties":{"workspace_id":{"type":"string"},"name":{"type":"string"},"branch":{"type":"string"},"base_ref":{"type":"string"},"task_id":{"type":["string","null"]},"task":{"type":["string","null"]}},"required":["workspace_id","name","branch","base_ref","task_id","task"],"additionalProperties":false}),
        ),
        _ => return None,
    })
}

fn review_handoff_output_schema() -> Value {
    json!({"type":"object","properties":{"workspace_id":{"type":"string"},"notes":{"type":"array","items":{"type":"object","properties":{"note_id":{"type":"string"},"ts_unix_ms":{"type":"integer"},"session_id":{"type":["string","null"]},"workspace_id":{"type":"string"},"title":{"type":"string"},"severity":{"type":"string"},"path":{"type":["string","null"]},"body":{"type":"string"}},"required":["note_id","ts_unix_ms","workspace_id","title","severity","path","body"],"additionalProperties":false}},"edit_plans":{"type":"array","items":{"type":"object","properties":{"created_unix_ms":{"type":"integer"},"updated_unix_ms":{"type":"integer"},"session_id":{"type":"string"},"workspace_id":{"type":"string"},"plan_id":{"type":"string"},"status":{"type":"string"},"title":{"type":"string"},"intent":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}},"patch_chars":{"type":"integer"},"files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}},"status_note":{"type":"string"},"approved_unix_ms":{"type":"integer"},"applied_unix_ms":{"type":"integer"},"applied_session_id":{"type":"string"},"applied_files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}}},"required":["created_unix_ms","updated_unix_ms","workspace_id","plan_id","status","title","intent","paths","files","applied_files"],"additionalProperties":false}},"truncated":{"type":"boolean"}},"required":["workspace_id","notes","edit_plans","truncated"],"additionalProperties":false})
}

fn pull_requests_handoff_output_schema() -> Value {
    json!({"type":"object","properties":{"workspace_id":{"type":["string","null"]},"branch":{"type":["string","null"]},"pull_requests":{"type":"array","items":{"type":"object","properties":{"created_unix_ms":{"type":"integer"},"updated_unix_ms":{"type":"integer"},"session_id":{"type":"string"},"workspace_id":{"type":"string"},"branch":{"type":"string"},"base":{"type":"string"},"title":{"type":"string"},"draft":{"type":"boolean"},"status":{"type":"string"},"url":{"type":"string"},"number":{"type":"integer"},"remote_state":{"type":"string"},"merged":{"type":"boolean"},"exit_code":{"type":"integer"},"body_chars":{"type":"integer"}},"required":["created_unix_ms","updated_unix_ms","workspace_id","branch","title","draft","status","body_chars"],"additionalProperties":false}},"status_counts":{"type":"object","additionalProperties":{"type":"integer"}},"truncated":{"type":"boolean"}},"required":["workspace_id","branch","pull_requests","status_counts","truncated"],"additionalProperties":false})
}

fn refresh_pull_requests_output_schema() -> Value {
    json!({"type":"object","properties":{"workspace_id":{"type":"string"},"branch":{"type":["string","null"]},"refreshed":{"type":"array","items":{"type":"object","properties":{"success":{"type":"boolean"},"selector":{"type":"string"},"exit_code":{"type":["integer","null"]},"timed_out":{"type":"boolean"},"stdout":{"type":"string"},"stderr":{"type":"string"},"truncated":{"type":"boolean"},"pull_request":{"type":["object","null"],"properties":{"created_unix_ms":{"type":"integer"},"updated_unix_ms":{"type":"integer"},"session_id":{"type":"string"},"workspace_id":{"type":"string"},"branch":{"type":"string"},"base":{"type":"string"},"title":{"type":"string"},"draft":{"type":"boolean"},"status":{"type":"string"},"url":{"type":"string"},"number":{"type":"integer"},"remote_state":{"type":"string"},"merged":{"type":"boolean"},"exit_code":{"type":"integer"},"body_chars":{"type":"integer"}},"required":["created_unix_ms","updated_unix_ms","workspace_id","branch","title","draft","status","body_chars"],"additionalProperties":false}},"required":["success","selector","exit_code","timed_out","stdout","stderr","truncated","pull_request"],"additionalProperties":false}},"succeeded":{"type":"integer"},"failed":{"type":"integer"},"truncated":{"type":"boolean"}},"required":["workspace_id","branch","refreshed","succeeded","failed","truncated"],"additionalProperties":false})
}

fn edit_plans_handoff_output_schema() -> Value {
    json!({"type":"object","properties":{"workspace_id":{"type":["string","null"]},"status":{"type":["string","null"]},"edit_plans":{"type":"array","items":{"type":"object","properties":{"created_unix_ms":{"type":"integer"},"updated_unix_ms":{"type":"integer"},"session_id":{"type":"string"},"workspace_id":{"type":"string"},"plan_id":{"type":"string"},"status":{"type":"string"},"title":{"type":"string"},"intent":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}},"patch_chars":{"type":"integer"},"files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}},"status_note":{"type":"string"},"approved_unix_ms":{"type":"integer"},"applied_unix_ms":{"type":"integer"},"applied_session_id":{"type":"string"},"applied_files":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"operation":{"type":"string","enum":["add","modify","delete"]},"bytes_before":{"type":"integer"},"bytes_after":{"type":"integer"}},"required":["path","operation","bytes_before","bytes_after"],"additionalProperties":false}}},"required":["created_unix_ms","updated_unix_ms","workspace_id","plan_id","status","title","intent","paths","files","applied_files"],"additionalProperties":false}},"status_counts":{"type":"object","additionalProperties":{"type":"integer"}},"truncated":{"type":"boolean"}},"required":["workspace_id","status","edit_plans","status_counts","truncated"],"additionalProperties":false})
}

fn tool_def(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": format!("[readonly] {description}"),
        "inputSchema": input_schema,
        "outputSchema": output_schema,
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "openWorldHint": false
        }
    })
}

fn apps_widget_tool_def(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
) -> Value {
    let mut def = tool_def(name, title, description, input_schema, output_schema);
    let resource_uri = match name {
        "render_review" => REVIEW_WIDGET_URI,
        "render_pull_requests" => PULL_REQUESTS_WIDGET_URI,
        "render_edit_plans" => EDIT_PLANS_WIDGET_URI,
        _ => CHANGES_WIDGET_URI,
    };
    if let Some(map) = def.as_object_mut() {
        map.insert(
            "_meta".to_string(),
            json!({
                "openai/outputTemplate": resource_uri,
                "ui": {"resourceUri": resource_uri}
            }),
        );
    }
    def
}

fn review_tool_def(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": format!("[review] {description}"),
        "inputSchema": input_schema,
        "outputSchema": output_schema,
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": false,
            "openWorldHint": false
        }
    })
}

fn mutating_tool_def(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": format!("[execute] {description}"),
        "inputSchema": input_schema,
        "outputSchema": output_schema,
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": true,
            "openWorldHint": false
        }
    })
}

fn network_tool_def(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": format!("[execute] {description}"),
        "inputSchema": input_schema,
        "outputSchema": output_schema,
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": false,
            "openWorldHint": true
        }
    })
}

fn rpc_result(id: Option<Value>, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": payload})
}

fn rpc_error(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": {"code": code, "message": message}})
}

fn json_rpc_response(status: StatusCode, body: Value, session_id: Option<&str>) -> Response {
    let payload = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, payload.len().to_string())
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(session_id) = session_id {
        builder = builder.header("Mcp-Session-Id", session_id);
    }
    builder.body(Body::from(payload)).unwrap()
}

fn json_response(status: StatusCode, body: Value) -> Response {
    let payload = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, payload.len().to_string())
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(payload))
        .unwrap()
}

fn html_response(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_LENGTH, body.len().to_string())
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn redirect_response(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(header::CONTENT_LENGTH, "0")
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .unwrap()
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    json_response(
        status,
        json!({"error": error, "error_description": description}),
    )
}

fn unauthorized_rpc_response(config: &Config) -> Response {
    let mut response = json_rpc_response(
        StatusCode::UNAUTHORIZED,
        rpc_error(None, -32001, "unauthorized"),
        None,
    );
    let metadata_url = public_url(config, PROTECTED_RESOURCE_METADATA_ENDPOINT);
    let challenge = format!(
        r#"Bearer resource_metadata="{}""#,
        metadata_url.replace('"', "%22")
    );
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

fn empty_response(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, "0")
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .unwrap()
}

async fn method_not_allowed() -> Response {
    let mut response = empty_response(StatusCode::METHOD_NOT_ALLOWED);
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

async fn not_found() -> Response {
    json_rpc_response(
        StatusCode::NOT_FOUND,
        rpc_error(None, -32601, "not found"),
        None,
    )
}

fn content_type(headers: &HeaderMap) -> &str {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or("")
}

fn header_string(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(header) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return state.config.owner_token.is_none();
    };
    let Some(presented) = header.strip_prefix("Bearer ") else {
        return state.config.owner_token.is_none();
    };
    if state
        .config
        .owner_token
        .as_ref()
        .map(|token| constant_time_eq(presented, token))
        .unwrap_or(false)
    {
        return true;
    }
    if state
        .oauth
        .as_ref()
        .map(|oauth| oauth_access_token_valid(oauth, presented))
        .unwrap_or(false)
    {
        return true;
    }
    state.config.owner_token.is_none()
}

fn authorized_for_scope(state: &AppState, headers: &HeaderMap, required: &str) -> bool {
    let Some(header) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(presented) = header.strip_prefix("Bearer ") else {
        return false;
    };
    if state
        .config
        .owner_token
        .as_ref()
        .map(|token| constant_time_eq(presented, token))
        .unwrap_or(false)
    {
        return true;
    }
    state
        .oauth
        .as_ref()
        .and_then(|oauth| oauth_access_token_scope(oauth, presented))
        .map(|scope| scope_includes(&scope, required))
        .unwrap_or(false)
}

fn host_ok(config: &Config, headers: &HeaderMap) -> bool {
    let Some(public_base_url) = &config.public_base_url else {
        return true;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let normalized = host.trim().to_ascii_lowercase();
    if normalized.starts_with("127.0.0.1:")
        || normalized == "127.0.0.1"
        || normalized.starts_with("localhost:")
        || normalized == "localhost"
        || normalized.starts_with("[::1]:")
        || normalized == "[::1]"
    {
        return true;
    }
    let Ok(url) = url::Url::parse(public_base_url) else {
        return false;
    };
    let Some(allowed_host) = url.host_str() else {
        return false;
    };
    let allowed = match url.port() {
        Some(port) => format!("{}:{}", allowed_host.to_ascii_lowercase(), port),
        None => allowed_host.to_ascii_lowercase(),
    };
    normalized == allowed
}

fn origin_ok(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    matches!(
        url.host_str().unwrap_or("").to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1" | "[::1]"
    )
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

fn truncate_bytes(value: &str) -> String {
    if value.len() <= MAX_READ_BYTES {
        return value.to_string();
    }
    String::from_utf8_lossy(&value.as_bytes()[..MAX_READ_BYTES]).to_string()
}

fn read_bounded_text(path: &Path, limit: usize) -> Result<(String, bool)> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

fn bounded_text_diff(before: &str, after: &str) -> (String, bool) {
    let mut diff = String::new();
    diff.push_str("--- before\n+++ after\n@@\n");
    diff.push_str("- ");
    diff.push_str(before);
    if !before.ends_with('\n') {
        diff.push('\n');
    }
    diff.push_str("+ ");
    diff.push_str(after);
    if !after.ends_with('\n') {
        diff.push('\n');
    }
    let truncated = diff.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut end = MAX_DIFF_BYTES;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
    }
    (diff, truncated)
}

fn truncate_string(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_project() -> PathBuf {
        let dir = env::temp_dir().join(format!("codex-connector-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn raw_config(root: PathBuf) -> RawConfig {
        RawConfig {
            allowed_roots: vec![root],
            skill_roots: vec![],
            trust_level: TrustLevel::Readonly,
            host: "127.0.0.1".to_string(),
            port: 0,
            owner_token: Some("secret".to_string()),
            public_base_url: None,
            state_dir: None,
            auto_skill_roots: false,
        }
    }

    fn init_options_for_test(config_path: PathBuf) -> InitOptions {
        InitOptions {
            config_path,
            roots: vec![],
            trust_level: TrustLevel::Readonly,
            host: "127.0.0.1".to_string(),
            port: 8765,
            owner_token: None,
            no_owner_token: false,
            public_base_url: None,
            state_dir: None,
            skill_roots: vec![],
            auto_skill_roots: true,
            no_interactive: false,
            force: false,
        }
    }

    #[test]
    fn prompt_init_options_collects_first_run_answers() {
        let root_a = temp_project();
        let root_b = temp_project();
        let skill_root = temp_project();
        let config_path =
            env::temp_dir().join(format!("codex-connector-prompt-{}.json", Uuid::new_v4()));
        let input = format!(
            "{},{}\n9876\nhttps://example.trycloudflare.com/\n{}\n",
            root_a.display(),
            root_b.display(),
            skill_root.display()
        );
        let mut input = std::io::Cursor::new(input.into_bytes());
        let mut output = Vec::new();
        let prompted =
            prompt_init_options(init_options_for_test(config_path), &mut input, &mut output)
                .unwrap();

        assert_eq!(prompted.roots, vec![root_a.clone(), root_b.clone()]);
        assert_eq!(prompted.port, 9876);
        assert_eq!(
            prompted.public_base_url.as_deref(),
            Some("https://example.trycloudflare.com")
        );
        assert_eq!(prompted.skill_roots, vec![skill_root.clone()]);
        assert!(prompted.auto_skill_roots);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Codex connector setup"));
        assert!(output.contains("Allowed project roots"));
        fs::remove_dir_all(root_a).unwrap();
        fs::remove_dir_all(root_b).unwrap();
        fs::remove_dir_all(skill_root).unwrap();
    }

    #[test]
    fn prompt_init_options_accepts_defaults() {
        let config_path =
            env::temp_dir().join(format!("codex-connector-prompt-{}.json", Uuid::new_v4()));
        let mut options = init_options_for_test(config_path);
        options.skill_roots = vec![PathBuf::from("/explicit/skills")];
        options.public_base_url = Some("https://bridge.example".to_string());
        let mut input = std::io::Cursor::new(b"\n8766\n".to_vec());
        let mut output = Vec::new();
        let prompted = prompt_init_options(options, &mut input, &mut output).unwrap();

        assert_eq!(prompted.roots, vec![env::current_dir().unwrap()]);
        assert_eq!(prompted.port, 8766);
        assert_eq!(
            prompted.public_base_url.as_deref(),
            Some("https://bridge.example")
        );
        assert_eq!(
            prompted.skill_roots,
            vec![PathBuf::from("/explicit/skills")]
        );
        assert!(prompted.auto_skill_roots);
    }

    #[test]
    fn prompt_init_none_disables_auto_skill_roots() {
        let root = temp_project();
        let config_path =
            env::temp_dir().join(format!("codex-connector-prompt-{}.json", Uuid::new_v4()));
        let input = format!("{}\n8765\n\nnone\n", root.display());
        let mut input = std::io::Cursor::new(input.into_bytes());
        let mut output = Vec::new();
        let prompted =
            prompt_init_options(init_options_for_test(config_path), &mut input, &mut output)
                .unwrap();

        assert!(prompted.skill_roots.is_empty());
        assert!(!prompted.auto_skill_roots);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn optional_path_list_accepts_none() {
        assert_eq!(
            parse_optional_path_list_with_default("none", &[PathBuf::from("/default")]).unwrap(),
            Vec::<PathBuf>::new()
        );
        assert_eq!(
            parse_optional_path_list_with_default("", &[PathBuf::from("/default")]).unwrap(),
            vec![PathBuf::from("/default")]
        );
        assert_eq!(
            parse_optional_path_list_with_default("", &[]).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    fn init_git_repo(root: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(root)
            .status()
            .unwrap();
        fs::write(root.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
    }

    async fn raw_http(addr: SocketAddr, request: String) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    }

    fn http_status(response: &str) -> u16 {
        response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .unwrap()
    }

    async fn post_json(addr: SocketAddr, body: Value, session_id: Option<&str>) -> (String, Value) {
        let payload = body.to_string();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAuthorization: Bearer secret\r\nContent-Length: {}\r\nConnection: close\r\n",
            payload.len()
        );
        if let Some(session_id) = session_id {
            request.push_str(&format!("Mcp-Session-Id: {session_id}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&payload);

        let response = raw_http(addr, request).await;
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
        (response.clone(), serde_json::from_str(body).unwrap())
    }

    async fn get_raw(addr: SocketAddr, path: &str, host: &str) -> String {
        raw_http(
            addr,
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
        )
        .await
    }

    async fn post_form(
        addr: SocketAddr,
        path: &str,
        host: &str,
        params: &[(&str, &str)],
    ) -> String {
        let payload = form_encode(params);
        raw_http(
            addr,
            format!(
                "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            ),
        )
        .await
    }

    fn form_encode(params: &[(&str, &str)]) -> String {
        let mut encoded = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in params {
            encoded.append_pair(key, value);
        }
        encoded.finish()
    }

    fn http_body(response: &str) -> &str {
        response.split("\r\n\r\n").nth(1).unwrap_or("")
    }

    fn s256(verifier: &str) -> String {
        let digest = Sha256::digest(verifier.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    }

    fn header_value(response: &str, header: &str) -> Option<String> {
        let needle = header.to_ascii_lowercase();
        response.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.to_ascii_lowercase() == needle {
                Some(value.trim().to_string())
            } else {
                None
            }
        })
    }

    #[test]
    fn validate_rejects_public_base_url_with_mcp_path() {
        let root = temp_project();
        let mut raw = raw_config(root.clone());
        raw.public_base_url = Some("https://example.com/mcp".to_string());
        let err = validate_raw(raw).unwrap_err().to_string();
        assert!(err.contains("without /mcp"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_rejects_cleartext_public_base_url() {
        let root = temp_project();
        let mut raw = raw_config(root.clone());
        raw.public_base_url = Some("http://example.com".to_string());
        let err = validate_raw(raw).unwrap_err().to_string();
        assert!(err.contains("must use https"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_allows_loopback_http_public_base_url() {
        let root = temp_project();
        let mut raw = raw_config(root.clone());
        raw.public_base_url = Some("http://127.0.0.1:8765".to_string());
        let config = validate_raw(raw).unwrap();
        assert_eq!(
            config.public_base_url.as_deref(),
            Some("http://127.0.0.1:8765")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_rejects_execute_without_owner_token() {
        let root = temp_project();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.owner_token = None;
        let err = validate_raw(raw).unwrap_err().to_string();
        assert!(err.contains("owner_token is required"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tool_definitions_are_readonly_annotated() {
        let tools = tool_definitions(TrustLevel::Readonly);
        assert_eq!(tools.len(), 21);
        for tool in tools {
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
            assert_eq!(tool["annotations"]["destructiveHint"], false);
            assert_eq!(tool["annotations"]["openWorldHint"], false);
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["outputSchema"]["type"], "object");
        }
        let render = tool_definitions(TrustLevel::Readonly)
            .into_iter()
            .find(|tool| tool["name"] == "render_changes")
            .unwrap();
        assert_eq!(render["_meta"]["openai/outputTemplate"], CHANGES_WIDGET_URI);
        let render_review = tool_definitions(TrustLevel::Readonly)
            .into_iter()
            .find(|tool| tool["name"] == "render_review")
            .unwrap();
        assert_eq!(
            render_review["_meta"]["openai/outputTemplate"],
            REVIEW_WIDGET_URI
        );
        let render_pull_requests = tool_definitions(TrustLevel::Readonly)
            .into_iter()
            .find(|tool| tool["name"] == "render_pull_requests")
            .unwrap();
        assert_eq!(
            render_pull_requests["_meta"]["openai/outputTemplate"],
            PULL_REQUESTS_WIDGET_URI
        );
        let render_edit_plans = tool_definitions(TrustLevel::Readonly)
            .into_iter()
            .find(|tool| tool["name"] == "render_edit_plans")
            .unwrap();
        assert_eq!(
            render_edit_plans["_meta"]["openai/outputTemplate"],
            EDIT_PLANS_WIDGET_URI
        );
        let preview = tool_definitions(TrustLevel::Readonly)
            .into_iter()
            .find(|tool| tool["name"] == "preview_patch")
            .unwrap();
        assert_eq!(preview["annotations"]["readOnlyHint"], true);
        assert_eq!(preview["annotations"]["destructiveHint"], false);
    }

    #[test]
    fn execute_tool_definitions_include_mutating_tools() {
        let tools = tool_definitions(TrustLevel::Execute);
        let names: HashSet<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains("show_changes"));
        assert!(names.contains("render_changes"));
        assert!(names.contains("show_review"));
        assert!(names.contains("render_review"));
        assert!(names.contains("list_worktrees"));
        assert!(names.contains("list_pull_requests"));
        assert!(names.contains("show_pull_requests"));
        assert!(names.contains("render_pull_requests"));
        assert!(names.contains("list_notes"));
        assert!(names.contains("list_edit_plans"));
        assert!(names.contains("show_edit_plans"));
        assert!(names.contains("render_edit_plans"));
        assert!(names.contains("preview_patch"));
        assert!(names.contains("write"));
        assert!(names.contains("edit"));
        assert!(names.contains("apply_patch"));
        assert!(names.contains("move_path"));
        assert!(names.contains("shell"));
        assert!(names.contains("open_worktree"));
        assert!(names.contains("publish_branch"));
        assert!(names.contains("create_pull_request"));
        assert!(names.contains("refresh_pull_request_status"));
        assert!(names.contains("refresh_pull_requests"));
        assert!(names.contains("create_note"));
        assert!(names.contains("create_edit_plan"));
        assert!(names.contains("update_edit_plan_status"));
        let write = tools.iter().find(|tool| tool["name"] == "write").unwrap();
        assert_eq!(write["annotations"]["readOnlyHint"], false);
        assert_eq!(write["annotations"]["destructiveHint"], true);
        let publish = tools
            .iter()
            .find(|tool| tool["name"] == "publish_branch")
            .unwrap();
        assert_eq!(publish["annotations"]["readOnlyHint"], false);
        assert_eq!(publish["annotations"]["destructiveHint"], true);
        let create_pr = tools
            .iter()
            .find(|tool| tool["name"] == "create_pull_request")
            .unwrap();
        assert_eq!(create_pr["annotations"]["readOnlyHint"], false);
        assert_eq!(create_pr["annotations"]["destructiveHint"], false);
        assert_eq!(create_pr["annotations"]["openWorldHint"], true);
        assert!(create_pr["outputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("title"));
        assert!(create_pr["outputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("body_chars"));
        let refresh = tools
            .iter()
            .find(|tool| tool["name"] == "refresh_pull_request_status")
            .unwrap();
        assert_eq!(refresh["annotations"]["readOnlyHint"], false);
        assert_eq!(refresh["annotations"]["destructiveHint"], false);
        assert_eq!(refresh["annotations"]["openWorldHint"], true);
        let refresh_many = tools
            .iter()
            .find(|tool| tool["name"] == "refresh_pull_requests")
            .unwrap();
        assert_eq!(refresh_many["annotations"]["readOnlyHint"], false);
        assert_eq!(refresh_many["annotations"]["destructiveHint"], false);
        assert_eq!(refresh_many["annotations"]["openWorldHint"], true);
    }

    #[test]
    fn execute_tools_require_oauth_execute_scopes() {
        for tool in [
            "write",
            "edit",
            "apply_patch",
            "move_path",
            "open_worktree",
            "publish_branch",
            "create_pull_request",
            "refresh_pull_request_status",
            "refresh_pull_requests",
        ] {
            assert_eq!(required_tool_scope(tool), Some("workspace:write"), "{tool}");
        }
        assert_eq!(required_tool_scope("shell"), Some("shell"));
    }

    #[test]
    fn review_tool_definitions_include_create_note_without_execute_tools() {
        let tools = tool_definitions(TrustLevel::Review);
        let names: HashSet<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains("create_note"));
        assert!(names.contains("create_edit_plan"));
        assert!(names.contains("update_edit_plan_status"));
        assert!(names.contains("read"));
        assert!(names.contains("preview_patch"));
        assert!(names.contains("list_notes"));
        assert!(names.contains("list_edit_plans"));
        assert!(!names.contains("write"));
        assert!(!names.contains("move_path"));
        assert!(!names.contains("shell"));
        assert!(!names.contains("open_worktree"));
        let note = tools
            .iter()
            .find(|tool| tool["name"] == "create_note")
            .unwrap();
        assert_eq!(note["annotations"]["readOnlyHint"], false);
        assert_eq!(note["annotations"]["destructiveHint"], false);
        let edit_plan = tools
            .iter()
            .find(|tool| tool["name"] == "create_edit_plan")
            .unwrap();
        assert_eq!(edit_plan["annotations"]["readOnlyHint"], false);
        assert_eq!(edit_plan["annotations"]["destructiveHint"], false);
        let edit_plan_status = tools
            .iter()
            .find(|tool| tool["name"] == "update_edit_plan_status")
            .unwrap();
        assert_eq!(edit_plan_status["annotations"]["readOnlyHint"], false);
        assert_eq!(edit_plan_status["annotations"]["destructiveHint"], false);
    }

    #[test]
    fn resolve_path_rejects_parent_traversal() {
        let root = temp_project();
        fs::write(root.join("a.txt"), "hello").unwrap();
        let ws = Workspace {
            root: root.clone(),
            activated_skill_dirs: HashSet::new(),
        };
        let err = resolve_file(&ws, "../a.txt").unwrap_err().to_string();
        assert!(err.contains("may not contain '..'"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_file_rejects_symlink_escape() {
        let root = temp_project();
        let outside = temp_project();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("leak.txt")).unwrap();
        let ws = Workspace {
            root: root.clone(),
            activated_skill_dirs: HashSet::new(),
        };
        let err = resolve_file(&ws, "leak.txt").unwrap_err().to_string();
        assert!(err.contains("symlink"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn open_workspace_caps_registry_size() {
        let root = temp_project();
        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let first = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let first_id = first["workspace_id"].as_str().unwrap().to_string();
        for _ in 0..MAX_OPEN_WORKSPACES {
            open_workspace(&state, root.to_str().unwrap()).unwrap();
        }
        let registry = state.registry.lock().unwrap();
        assert_eq!(registry.workspaces.len(), MAX_OPEN_WORKSPACES);
        assert!(!registry.workspaces.contains_key(&first_id));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_workspace_returns_bounded_project_instructions() {
        let root = temp_project();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("AGENTS.md"), "agent guidance").unwrap();
        fs::write(root.join("CONTEXT.md"), "context guidance").unwrap();
        fs::write(root.join("nested").join("AGENTS.md"), "nested guidance").unwrap();
        fs::write(root.join("target").join("AGENTS.md"), "ignored guidance").unwrap();
        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let instructions = opened["instructions"].as_array().unwrap();
        assert_eq!(instructions.len(), 2);
        assert_eq!(instructions[0]["path"], "AGENTS.md");
        assert_eq!(instructions[0]["content"], "agent guidance");
        let available = opened["available_instructions"].as_array().unwrap();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0]["path"], "nested/AGENTS.md");
        assert_eq!(opened["available_instructions_truncated"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn list_skills_reads_configured_skill_roots() {
        let root = temp_project();
        let skill_root = temp_project();
        let skill_dir = skill_root.join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\n",
        )
        .unwrap();
        let mut raw = raw_config(root.clone());
        raw.skill_roots = vec![skill_root.clone()];
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let listed = list_skills_tool(&state).unwrap();
        assert_eq!(listed["skills"][0]["name"], "demo-skill");
        assert_eq!(listed["skills"][0]["description"], "Use for demo tasks.");
        assert_eq!(listed["skills"][0]["path"], "demo/SKILL.md");
        assert!(listed["skills"][0]["entrypoint"]
            .as_str()
            .unwrap()
            .starts_with("skill://"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(skill_root).unwrap();
    }

    #[test]
    fn open_workspace_auto_discovers_project_skill_roots() {
        let root = temp_project();
        let pi_skill_dir = root.join(".pi").join("skills").join("demo-pi");
        let project_skill_dir = root.join("skills").join("demo-project");
        fs::create_dir_all(&pi_skill_dir).unwrap();
        fs::create_dir_all(&project_skill_dir).unwrap();
        fs::write(
            pi_skill_dir.join("SKILL.md"),
            "---\nname: pi-skill\ndescription: Use project-local pi skills.\n---\n# PI Skill\n",
        )
        .unwrap();
        fs::write(
            project_skill_dir.join("SKILL.md"),
            "---\nname: project-skill\ndescription: Use project skills.\n---\n# Project Skill\n",
        )
        .unwrap();
        let mut raw = raw_config(root.clone());
        raw.auto_skill_roots = true;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let names = opened["skills"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|skill| skill["name"].as_str())
            .collect::<HashSet<_>>();
        assert!(names.contains("pi-skill"));
        assert!(names.contains("project-skill"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn auto_skill_roots_can_be_disabled() {
        let root = temp_project();
        let skill_dir = root.join("skills").join("disabled-demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: disabled-skill\ndescription: Hidden when auto discovery is off.\n---\n# Hidden\n",
        )
        .unwrap();
        let mut raw = raw_config(root.clone());
        raw.auto_skill_roots = false;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        assert_eq!(opened["skills"].as_array().unwrap().len(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn auto_skill_roots_ignore_workspace_symlink_escapes() {
        let root = temp_project();
        let external = temp_project();
        let external_skill = external.join("leaked");
        fs::create_dir_all(&external_skill).unwrap();
        fs::write(
            external_skill.join("SKILL.md"),
            "---\nname: leaked-skill\ndescription: Outside workspace.\n---\n# Leaked\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&external, root.join("skills")).unwrap();
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(root.join("skills")).unwrap();
        }
        let mut raw = raw_config(root.clone());
        raw.auto_skill_roots = true;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        assert!(opened["skills"].as_array().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(external).unwrap();
    }

    #[test]
    fn existing_configs_without_auto_skill_roots_default_to_workspace_local_discovery() {
        let root = temp_project();
        let skill_dir = root.join(".pi").join("skills").join("upgrade-demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: upgrade-skill\ndescription: Existing config default.\n---\n# Upgrade\n",
        )
        .unwrap();
        let raw: RawConfig = serde_json::from_value(json!({
            "allowed_roots": [root],
            "trust_level": "readonly",
            "host": "127.0.0.1",
            "port": 0,
            "owner_token": "secret"
        }))
        .unwrap();
        let config = validate_raw(raw).unwrap();
        assert!(config.auto_skill_roots);
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened =
            open_workspace(&state, state.config.allowed_roots[0].to_str().unwrap()).unwrap();
        let names = opened["skills"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|skill| skill["name"].as_str())
            .collect::<HashSet<_>>();
        assert!(names.contains("upgrade-skill"));
        fs::remove_dir_all(state.config.allowed_roots[0].clone()).unwrap();
    }

    #[test]
    fn skill_resources_require_reading_skill_file_first() {
        let root = temp_project();
        let skill_root = temp_project();
        let skill_dir = skill_root.join("demo");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\n",
        )
        .unwrap();
        fs::write(skill_dir.join("references").join("guide.md"), "reference").unwrap();
        let mut raw = raw_config(root.clone());
        raw.skill_roots = vec![skill_root.clone()];
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let entrypoint = opened["skills"][0]["entrypoint"]
            .as_str()
            .unwrap()
            .to_string();
        let skill_id = opened["skills"][0]["skill_id"]
            .as_str()
            .unwrap()
            .to_string();
        let resource_uri = format!("skill://{skill_id}/references/guide.md");
        let resource_before_skill = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!(resource_uri)),
            ]),
        )
        .unwrap_err()
        .to_string();
        assert!(resource_before_skill.contains("before reading other files"));
        let skill_read = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                (
                    "workspace_id".to_string(),
                    json!(opened["workspace_id"].as_str().unwrap()),
                ),
                ("path".to_string(), json!(entrypoint)),
            ]),
        )
        .unwrap();
        assert!(skill_read["content"].as_str().unwrap().contains("# Demo"));
        let resource_after_skill = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                (
                    "workspace_id".to_string(),
                    json!(opened["workspace_id"].as_str().unwrap()),
                ),
                (
                    "path".to_string(),
                    json!(format!("skill://{skill_id}/references/guide.md")),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(resource_after_skill["content"], "reference");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(skill_root).unwrap();
    }

    #[test]
    fn workspace_relative_skill_paths_use_same_entrypoint_guard() {
        let root = temp_project();
        let skill_root = root.join("skills");
        let skill_dir = skill_root.join("demo");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\n",
        )
        .unwrap();
        fs::write(skill_dir.join("references").join("guide.md"), "reference").unwrap();
        let mut raw = raw_config(root.clone());
        raw.auto_skill_roots = true;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let blocked = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("skills/demo/references/guide.md")),
            ]),
        )
        .unwrap_err()
        .to_string();
        assert!(blocked.contains("before reading other files"));
        let skill_read = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                (
                    "workspace_id".to_string(),
                    json!(opened["workspace_id"].as_str().unwrap()),
                ),
                ("path".to_string(), json!("skills/demo/SKILL.md")),
            ]),
        )
        .unwrap();
        assert!(skill_read["content"].as_str().unwrap().contains("# Demo"));
        let resource_after_skill = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                (
                    "workspace_id".to_string(),
                    json!(opened["workspace_id"].as_str().unwrap()),
                ),
                ("path".to_string(), json!("skills/demo/references/guide.md")),
            ]),
        )
        .unwrap();
        assert_eq!(resource_after_skill["content"], "reference");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_file_tool_bounds_large_file_content() {
        let root = temp_project();
        fs::write(root.join("large.txt"), "x".repeat(MAX_READ_BYTES + 10)).unwrap();
        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let read = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("large.txt")),
            ]),
        )
        .unwrap();
        assert_eq!(read["content"].as_str().unwrap().len(), MAX_READ_BYTES);
        assert_eq!(read["truncated"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mutating_tools_are_hidden_and_rejected_in_readonly() {
        let root = temp_project();
        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let tools = tool_definitions(TrustLevel::Readonly);
        let tool_names: HashSet<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(!tool_names.contains("write"));
        assert!(!tool_names.contains("edit"));
        assert!(!tool_names.contains("apply_patch"));
        assert!(!tool_names.contains("move_path"));
        assert!(!tool_names.contains("shell"));
        assert!(!tool_names.contains("open_worktree"));
        assert!(!tool_names.contains("publish_branch"));
        assert!(!tool_names.contains("create_pull_request"));
        assert!(!tool_names.contains("create_note"));
        assert!(!tool_names.contains("create_edit_plan"));
        assert!(!tool_names.contains("update_edit_plan_status"));
        assert!(tool_names.contains("preview_patch"));
        assert!(tool_names.contains("show_changes"));
        assert!(tool_names.contains("list_worktrees"));
        assert!(tool_names.contains("list_pull_requests"));
        assert!(tool_names.contains("list_edit_plans"));
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let attempted = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"write","arguments":{"workspace_id":workspace_id,"path":"new.txt","content":"hello"}}),
            Some("sid_write"),
        );
        assert_eq!(attempted["result"]["isError"], true);
        assert!(attempted["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trust_level=execute"));
        let shell_attempt = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"shell","arguments":{"workspace_id":workspace_id,"command":"pwd"}}),
            Some("sid_shell"),
        );
        assert_eq!(shell_attempt["result"]["isError"], true);
        assert!(shell_attempt["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trust_level=execute"));
        let worktree_attempt = handle_tools_call(
            &state,
            Some(json!(3)),
            json!({"name":"open_worktree","arguments":{"workspace_id":workspace_id}}),
            Some("sid_worktree"),
        );
        assert_eq!(worktree_attempt["result"]["isError"], true);
        assert!(worktree_attempt["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trust_level=execute"));
        let publish_attempt = handle_tools_call(
            &state,
            Some(json!(4)),
            json!({"name":"publish_branch","arguments":{"workspace_id":workspace_id}}),
            Some("sid_publish"),
        );
        assert_eq!(publish_attempt["result"]["isError"], true);
        assert!(publish_attempt["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trust_level=execute"));
        let pr_attempt = handle_tools_call(
            &state,
            Some(json!(5)),
            json!({"name":"create_pull_request","arguments":{"workspace_id":workspace_id,"title":"PR","body":"secret pr body"}}),
            Some("sid_pr"),
        );
        assert_eq!(pr_attempt["result"]["isError"], true);
        assert!(pr_attempt["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trust_level=execute"));
        let note_attempt = handle_tools_call(
            &state,
            Some(json!(6)),
            json!({"name":"create_note","arguments":{"workspace_id":workspace_id,"title":"Finding","body":"secret review body"}}),
            Some("sid_note"),
        );
        assert_eq!(note_attempt["result"]["isError"], true);
        assert!(note_attempt["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("trust_level=review"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tool_result_includes_apps_compatible_meta() {
        let result = tool_result(
            "read",
            json!({"path":"README.md","content":"# Hello\n","truncated":false}),
            false,
        );
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["path"], "README.md");
        assert_eq!(result["_meta"]["codex-web-bridge/tool"], "read");
        assert_eq!(result["_meta"]["codex-web-bridge/is_error"], false);
        assert_eq!(
            result["_meta"]["codex-web-bridge/summary"]["content_chars"],
            8
        );
        assert_eq!(
            result["content"][0]["text"].as_str().unwrap(),
            "{\"content\":\"# Hello\\n\",\"path\":\"README.md\",\"truncated\":false}"
        );
    }

    #[test]
    fn shell_result_meta_does_not_duplicate_output_bodies() {
        let result = tool_result(
            "shell",
            json!({
                "cwd": ".",
                "exit_code": 0,
                "timed_out": false,
                "stdout": "secret stdout",
                "stderr": "secret stderr",
                "truncated": false
            }),
            false,
        );
        let meta_text = result["_meta"].to_string();
        assert_eq!(
            result["_meta"]["codex-web-bridge/summary"]["stdout_chars"],
            13
        );
        assert_eq!(
            result["_meta"]["codex-web-bridge/summary"]["stderr_chars"],
            13
        );
        assert!(!meta_text.contains("secret stdout"));
        assert!(!meta_text.contains("secret stderr"));
    }

    #[test]
    fn write_tool_creates_workspace_relative_file_in_execute_mode() {
        let root = temp_project();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let written = write_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("src/new.txt")),
                ("content".to_string(), json!("hello\n")),
            ]),
        )
        .unwrap_err();
        assert!(
            written.to_string().contains("No such file") || written.to_string().contains("not")
        );
        fs::create_dir(root.join("src")).unwrap();
        let written = write_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("src/new.txt")),
                ("content".to_string(), json!("hello\n")),
            ]),
        )
        .unwrap();
        assert_eq!(written["created"], true);
        assert_eq!(written["bytes_after"], 6);
        assert_eq!(
            fs::read_to_string(root.join("src/new.txt")).unwrap(),
            "hello\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_tool_applies_exact_match_with_count_guard() {
        let root = temp_project();
        fs::write(root.join("file.txt"), "one two one\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let ambiguous = edit_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("file.txt")),
                ("old_text".to_string(), json!("one")),
                ("new_text".to_string(), json!("ONE")),
            ]),
        )
        .unwrap_err();
        assert!(ambiguous.to_string().contains("multiple"));
        let edited = edit_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("file.txt")),
                ("old_text".to_string(), json!("one")),
                ("new_text".to_string(), json!("ONE")),
                ("replace_all".to_string(), json!(true)),
                ("expected_replacements".to_string(), json!(2)),
            ]),
        )
        .unwrap();
        assert_eq!(edited["replacements"], 2);
        assert_eq!(
            fs::read_to_string(root.join("file.txt")).unwrap(),
            "ONE two ONE\n"
        );
        let malformed_guard = edit_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("file.txt")),
                ("old_text".to_string(), json!("ONE")),
                ("new_text".to_string(), json!("one")),
                ("replace_all".to_string(), json!("true")),
            ]),
        )
        .unwrap_err();
        assert!(malformed_guard.to_string().contains("replace_all"));
        let malformed_count = edit_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("file.txt")),
                ("old_text".to_string(), json!("ONE")),
                ("new_text".to_string(), json!("one")),
                ("replace_all".to_string(), json!(true)),
                ("expected_replacements".to_string(), json!("2")),
            ]),
        )
        .unwrap_err();
        assert!(malformed_count
            .to_string()
            .contains("expected_replacements"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_path_tool_moves_and_optionally_overwrites_files() {
        let root = temp_project();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/old.txt"), "hello\n").unwrap();
        fs::write(root.join("src/existing.txt"), "existing\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let moved = move_path_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("from_path".to_string(), json!("src/old.txt")),
                ("to_path".to_string(), json!("src/new.txt")),
            ]),
        )
        .unwrap();
        assert_eq!(moved["from_path"], "src/old.txt");
        assert_eq!(moved["to_path"], "src/new.txt");
        assert_eq!(moved["overwritten"], false);
        assert_eq!(moved["bytes"], 6);
        assert!(!root.join("src/old.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("src/new.txt")).unwrap(),
            "hello\n"
        );
        let overwrite_guard = move_path_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("from_path".to_string(), json!("src/new.txt")),
                ("to_path".to_string(), json!("src/existing.txt")),
            ]),
        )
        .unwrap_err();
        assert!(overwrite_guard.to_string().contains("overwrite=true"));
        let overwritten = move_path_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("from_path".to_string(), json!("src/new.txt")),
                ("to_path".to_string(), json!("src/existing.txt")),
                ("overwrite".to_string(), json!(true)),
            ]),
        )
        .unwrap();
        assert_eq!(overwritten["overwritten"], true);
        assert_eq!(
            fs::read_to_string(root.join("src/existing.txt")).unwrap(),
            "hello\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_patch_validates_without_writing_files() {
        let root = temp_project();
        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 one
-two
+TWO
";
        let preview = preview_patch_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("patch".to_string(), json!(patch)),
            ]),
        )
        .unwrap();
        assert_eq!(preview["would_apply"], true);
        assert_eq!(preview["files"].as_array().unwrap().len(), 1);
        assert_eq!(preview["files"][0]["operation"], "modify");
        assert!(preview["diff"].as_str().unwrap().contains("TWO"));
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_patch_tool_applies_unified_diff_atomically() {
        let root = temp_project();
        fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(root.join("b.txt"), "alpha\nbeta\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
--- a/b.txt
+++ b/b.txt
@@ -1,2 +1,3 @@
 alpha
+inserted
 beta
";
        let result = apply_patch_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("patch".to_string(), json!(patch)),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("b.txt")).unwrap(),
            "alpha\ninserted\nbeta\n"
        );
        assert_eq!(result["files"].as_array().unwrap().len(), 2);
        assert!(result["diff"].as_str().unwrap().contains("TWO"));

        let bad_patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 one
-missing
+bad
 three
";
        let err = apply_patch_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("patch".to_string(), json!(bad_patch)),
            ]),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("mismatch"));
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_patch_tool_adds_and_deletes_files() {
        let root = temp_project();
        fs::write(root.join("delete.txt"), "remove me\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let patch = "\
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
--- a/delete.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-remove me
";
        let result = apply_patch_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("patch".to_string(), json!(patch)),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("new.txt")).unwrap(),
            "hello\nworld\n"
        );
        assert!(!root.join("delete.txt").exists());
        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["operation"], "add");
        assert_eq!(files[1]["operation"], "delete");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mutation_tools_reject_escapes_symlinks_and_binary_files() {
        let root = temp_project();
        let outside = temp_project();
        fs::write(root.join("binary.bin"), [0xff, 0x00, 0x41]).unwrap();
        fs::write(root.join("nul.txt"), b"hello\0world").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("leak.txt"), root.join("link.txt")).unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let escape = write_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("../escape.txt")),
                ("content".to_string(), json!("no")),
            ]),
        )
        .unwrap_err();
        assert!(escape.to_string().contains(".."));
        let move_escape = move_path_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("from_path".to_string(), json!("nul.txt")),
                ("to_path".to_string(), json!("../escape.txt")),
            ]),
        )
        .unwrap_err();
        assert!(move_escape.to_string().contains(".."));
        #[cfg(unix)]
        {
            let symlink = write_file_tool(
                &state,
                &serde_json::Map::from_iter([
                    ("workspace_id".to_string(), json!(workspace_id)),
                    ("path".to_string(), json!("link.txt")),
                    ("content".to_string(), json!("no")),
                ]),
            )
            .unwrap_err();
            assert!(symlink.to_string().contains("symlink"));
            let symlink_move = move_path_tool(
                &state,
                &serde_json::Map::from_iter([
                    ("workspace_id".to_string(), json!(workspace_id)),
                    ("from_path".to_string(), json!("link.txt")),
                    ("to_path".to_string(), json!("moved-link.txt")),
                ]),
            )
            .unwrap_err();
            assert!(symlink_move.to_string().contains("symlink"));
        }
        let binary = edit_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("binary.bin")),
                ("old_text".to_string(), json!("A")),
                ("new_text".to_string(), json!("B")),
            ]),
        )
        .unwrap_err();
        assert!(binary.to_string().contains("UTF-8"));
        let nul_file = edit_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("nul.txt")),
                ("old_text".to_string(), json!("hello")),
                ("new_text".to_string(), json!("hi")),
            ]),
        )
        .unwrap_err();
        assert!(nul_file.to_string().contains("binary"));
        let nul_content = write_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("path".to_string(), json!("new-nul.txt")),
                ("content".to_string(), json!("hello\0world")),
            ]),
        )
        .unwrap_err();
        assert!(nul_content.to_string().contains("binary"));
        let patch_escape = apply_patch_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                (
                    "patch".to_string(),
                    json!(
                        "--- a/../escape.txt\n+++ b/../escape.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"
                    ),
                ),
            ]),
            None,
        )
        .unwrap_err();
        assert!(patch_escape.to_string().contains(".."));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn shell_tool_runs_with_cwd_bounds_and_scrubbed_env() {
        let root = temp_project();
        fs::create_dir(root.join("src")).unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let output = shell_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("cwd".to_string(), json!("src")),
                ("command".to_string(), json!("printf '%s' \"$PWD\"")),
            ]),
        )
        .unwrap();
        assert_eq!(output["timed_out"], false);
        assert_eq!(output["exit_code"], 0);
        assert!(output["stdout"].as_str().unwrap().ends_with("/src"));
        let escaped = shell_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("cwd".to_string(), json!("..")),
                ("command".to_string(), json!("pwd")),
            ]),
        )
        .unwrap_err();
        assert!(escaped.to_string().contains(".."));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shell_tool_times_out_truncates_and_scrubs_env() {
        let root = temp_project();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let env_output = shell_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                (
                    "command".to_string(),
                    json!("printf '%s' \"${OPENAI_API_KEY-unset}\""),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(env_output["stdout"], "unset");
        let truncated = shell_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                (
                    "command".to_string(),
                    json!("python3 - <<'PY'\nprint('x' * 70000)\nPY"),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(truncated["truncated"], true);
        assert!(truncated["stdout"].as_str().unwrap().len() <= MAX_SHELL_OUTPUT_BYTES);
        let timed_out = shell_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("command".to_string(), json!("sleep 1")),
                ("timeout_ms".to_string(), json!(10)),
            ]),
        )
        .unwrap();
        assert_eq!(timed_out["timed_out"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publish_branch_pushes_current_branch_to_origin() {
        let root = temp_project();
        let remote = temp_project();
        init_git_repo(&root);
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(&remote)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote)
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["checkout", "-b", "codex/publish-test"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        fs::write(root.join("README.md"), "# Published\n").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "publish test"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();

        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_publish"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let published = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"publish_branch","arguments":{"workspace_id":workspace_id}}),
            Some("sid_publish"),
        );
        assert_eq!(published["result"]["isError"], false);
        assert_eq!(published["result"]["structuredContent"]["success"], true);
        assert_eq!(
            published["result"]["structuredContent"]["branch"],
            "codex/publish-test"
        );
        assert_eq!(
            published["result"]["_meta"]["codex-web-bridge/tool"],
            "publish_branch"
        );
        let remote_head = git(&remote, &["rev-parse", "refs/heads/codex/publish-test"]).unwrap();
        let local_head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(remote_head.trim(), local_head.trim());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(remote).unwrap();
    }

    #[test]
    fn create_pull_request_uses_gh_with_body_file() {
        let root = temp_project();
        let state_dir = temp_project();
        let fake_dir = temp_project();
        let args_file = fake_dir.join("args.txt");
        let fake_gh = fake_dir.join("gh");
        init_git_repo(&root);
        Command::new("git")
            .args(["checkout", "-b", "codex/pr-test"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        fs::write(
            &fake_gh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho 'https://github.com/example/repo/pull/7'\n",
                args_file.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&fake_gh).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            fs::set_permissions(&fake_gh, permissions).unwrap();
        }

        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let created = create_pull_request_tool_with_gh(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("title".to_string(), json!("Test PR")),
                ("body".to_string(), json!("secret pr body")),
                ("base".to_string(), json!("main")),
                ("draft".to_string(), json!(true)),
            ]),
            fake_gh.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(created["success"], true);
        assert_eq!(created["branch"], "codex/pr-test");
        assert_eq!(created["base"], "main");
        assert_eq!(created["draft"], true);
        assert_eq!(created["url"], "https://github.com/example/repo/pull/7");
        let args = fs::read_to_string(args_file).unwrap();
        assert!(args.contains("pr\ncreate"));
        assert!(args.contains("--head\ncodex/pr-test"));
        assert!(args.contains("--base\nmain"));
        assert!(args.contains("--draft"));
        let arg_lines = args.lines().collect::<Vec<_>>();
        let body_file = arg_lines
            .windows(2)
            .find_map(|pair| (pair[0] == "--body-file").then_some(pair[1]))
            .unwrap();
        assert_eq!(fs::read_to_string(body_file).unwrap(), "secret pr body");
        assert!(Path::new(body_file).starts_with(pr_bodies_root(&state_dir)));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
        fs::remove_dir_all(fake_dir).unwrap();
    }

    #[test]
    fn pull_request_handoff_records_are_queryable_without_body() {
        let root = temp_project();
        let state_dir = temp_project();
        let mut raw = raw_config(root.clone());
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let args = serde_json::Map::from_iter([("workspace_id".to_string(), json!(workspace_id))]);
        let payload = json!({
            "branch": "codex/pr-state",
            "base": "main",
            "title": "State PR",
            "draft": true,
            "exit_code": 0,
            "success": true,
            "timed_out": false,
            "url": "https://github.com/example/repo/pull/9",
            "body_chars": 19,
            "stdout": "https://github.com/example/repo/pull/9",
            "stderr": "",
            "truncated": false
        });
        record_pull_request_from_tool_result(
            &state,
            Some("sid_pr_state"),
            "create_pull_request",
            &args,
            &payload,
        );
        let listed = list_pull_requests_tool(
            &state,
            &serde_json::Map::from_iter([(
                "workspace_id".to_string(),
                json!(args["workspace_id"].as_str().unwrap()),
            )]),
        )
        .unwrap();
        let pull_requests = listed["pull_requests"].as_array().unwrap();
        assert_eq!(pull_requests.len(), 1);
        assert_eq!(pull_requests[0]["branch"], "codex/pr-state");
        assert_eq!(pull_requests[0]["status"], "created");
        assert_eq!(pull_requests[0]["body_chars"], 19);
        assert_eq!(
            pull_requests[0]["url"],
            "https://github.com/example/repo/pull/9"
        );
        let shown = show_pull_requests_tool(
            &state,
            &serde_json::Map::from_iter([(
                "workspace_id".to_string(),
                json!(args["workspace_id"].as_str().unwrap()),
            )]),
        )
        .unwrap();
        assert_eq!(shown["pull_requests"].as_array().unwrap().len(), 1);
        assert_eq!(shown["status_counts"]["created"], 1);
        let rendered = tool_result("render_pull_requests", shown, false);
        assert_eq!(
            rendered["_meta"]["openai/outputTemplate"],
            PULL_REQUESTS_WIDGET_URI
        );
        let raw_state = fs::read_to_string(state_dir.join("workspace_state.json")).unwrap();
        assert!(raw_state.contains("codex/pr-state"));
        assert!(!raw_state.contains("secret pr body"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn persisted_pull_request_defaults_keep_older_state_loadable() {
        let state_dir = temp_project();
        fs::write(
            state_dir.join("workspace_state.json"),
            r#"{
  "sessions": [],
  "pull_requests": [{
    "created_unix_ms": 1,
    "workspace_id": "workspace-old",
    "branch": "codex/old-pr",
    "title": "Old PR"
  }],
  "edit_plans": []
}"#,
        )
        .unwrap();
        let state = load_persisted_state(&state_dir).unwrap();
        assert_eq!(state.pull_requests.len(), 1);
        assert_eq!(state.pull_requests[0].updated_unix_ms, 0);
        assert_eq!(state.pull_requests[0].draft, false);
        assert_eq!(state.pull_requests[0].status, "unknown");
        assert_eq!(state.pull_requests[0].body_chars, 0);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn refresh_pull_request_status_updates_persisted_handoff_record() {
        let root = temp_project();
        let state_dir = temp_project();
        let fake_dir = temp_project();
        let args_file = fake_dir.join("args.txt");
        let fake_gh = fake_dir.join("gh");
        init_git_repo(&root);
        Command::new("git")
            .args(["checkout", "-b", "codex/pr-refresh"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        fs::write(
            &fake_gh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{{\"state\":\"CLOSED\",\"merged\":true,\"url\":\"https://github.com/example/repo/pull/11\",\"number\":11,\"title\":\"Refreshed PR\",\"baseRefName\":\"main\",\"headRefName\":\"codex/pr-refresh\",\"isDraft\":false}}'\n",
                args_file.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&fake_gh).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            fs::set_permissions(&fake_gh, permissions).unwrap();
        }

        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let create_args =
            serde_json::Map::from_iter([("workspace_id".to_string(), json!(workspace_id))]);
        let payload = json!({
            "branch": "codex/pr-refresh",
            "base": "main",
            "title": "Initial PR",
            "draft": true,
            "exit_code": 0,
            "success": true,
            "timed_out": false,
            "url": "https://github.com/example/repo/pull/11",
            "body_chars": 24,
            "stdout": "https://github.com/example/repo/pull/11",
            "stderr": "",
            "truncated": false
        });
        record_pull_request_from_tool_result(
            &state,
            Some("sid_pr_refresh"),
            "create_pull_request",
            &create_args,
            &payload,
        );

        let refreshed = refresh_pull_request_status_tool_with_gh(
            &state,
            &serde_json::Map::from_iter([(
                "workspace_id".to_string(),
                json!(create_args["workspace_id"].as_str().unwrap()),
            )]),
            fake_gh.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(refreshed["success"], true);
        assert_eq!(refreshed["pull_request"]["status"], "merged");
        assert_eq!(refreshed["pull_request"]["remote_state"], "closed");
        assert_eq!(refreshed["pull_request"]["number"], 11);
        assert_eq!(refreshed["pull_request"]["draft"], false);
        assert_eq!(refreshed["pull_request"]["title"], "Refreshed PR");
        let args = fs::read_to_string(args_file).unwrap();
        assert!(args.contains("pr\nview\ncodex/pr-refresh\n--json"));

        let listed = list_pull_requests_tool(
            &state,
            &serde_json::Map::from_iter([(
                "workspace_id".to_string(),
                json!(create_args["workspace_id"].as_str().unwrap()),
            )]),
        )
        .unwrap();
        let pull_requests = listed["pull_requests"].as_array().unwrap();
        assert_eq!(pull_requests.len(), 1);
        assert_eq!(pull_requests[0]["status"], "merged");
        assert_eq!(pull_requests[0]["merged"], true);
        assert_eq!(pull_requests[0]["remote_state"], "closed");
        assert_eq!(pull_requests[0]["body_chars"], 24);
        let raw_state = fs::read_to_string(state_dir.join("workspace_state.json")).unwrap();
        let raw_state_json: Value = serde_json::from_str(&raw_state).unwrap();
        assert_eq!(
            raw_state_json["pull_requests"][0]["status"],
            Value::String("merged".to_string())
        );
        assert!(!raw_state.contains("secret pr body"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
        fs::remove_dir_all(fake_dir).unwrap();
    }

    #[test]
    fn refresh_pull_requests_batch_updates_persisted_handoff_records() {
        let root = temp_project();
        let state_dir = temp_project();
        let fake_dir = temp_project();
        let args_file = fake_dir.join("args.txt");
        let fake_gh = fake_dir.join("gh");
        init_git_repo(&root);
        fs::write(
            &fake_gh,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$@" >> '{}'
case "$3" in
  *pull/21*)
    printf '%s\n' '{{"state":"OPEN","merged":false,"url":"https://github.com/example/repo/pull/21","number":21,"title":"Batch Open","baseRefName":"main","headRefName":"codex/pr-batch-open","isDraft":true}}'
    ;;
  *pull/22*)
    printf '%s\n' '{{"state":"CLOSED","merged":true,"url":"https://github.com/example/repo/pull/22","number":22,"title":"Batch Merged","baseRefName":"main","headRefName":"codex/pr-batch-merged","isDraft":false}}'
    ;;
  *)
    echo 'not found' >&2
    exit 1
    ;;
esac
"#,
                args_file.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&fake_gh).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            fs::set_permissions(&fake_gh, permissions).unwrap();
        }

        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let create_args =
            serde_json::Map::from_iter([("workspace_id".to_string(), json!(workspace_id))]);
        for payload in [
            json!({
                "branch": "codex/pr-batch-open",
                "base": "main",
                "title": "Initial Open",
                "draft": false,
                "exit_code": 0,
                "success": true,
                "timed_out": false,
                "url": "https://github.com/example/repo/pull/21",
                "body_chars": 21,
                "stdout": "https://github.com/example/repo/pull/21",
                "stderr": "",
                "truncated": false
            }),
            json!({
                "branch": "codex/pr-batch-open",
                "base": "main",
                "title": "Duplicate Open",
                "draft": false,
                "exit_code": 0,
                "success": true,
                "timed_out": false,
                "url": "https://github.com/example/repo/pull/21",
                "body_chars": 23,
                "stdout": "https://github.com/example/repo/pull/21",
                "stderr": "",
                "truncated": false
            }),
            json!({
                "branch": "codex/pr-batch-merged",
                "base": "main",
                "title": "Initial Merged",
                "draft": true,
                "exit_code": 0,
                "success": true,
                "timed_out": false,
                "url": "https://github.com/example/repo/pull/22",
                "body_chars": 22,
                "stdout": "https://github.com/example/repo/pull/22",
                "stderr": "",
                "truncated": false
            }),
            json!({
                "branch": "codex/pr-batch-fails",
                "base": "main",
                "title": "Initial Fails",
                "draft": false,
                "exit_code": 0,
                "success": true,
                "timed_out": false,
                "url": "https://github.com/example/repo/pull/23",
                "body_chars": 23,
                "stdout": "https://github.com/example/repo/pull/23",
                "stderr": "",
                "truncated": false
            }),
        ] {
            record_pull_request_from_tool_result(
                &state,
                Some("sid_pr_batch_refresh"),
                "create_pull_request",
                &create_args,
                &payload,
            );
            thread::sleep(Duration::from_millis(2));
        }

        let refreshed = refresh_pull_requests_tool_with_gh(
            &state,
            &serde_json::Map::from_iter([
                (
                    "workspace_id".to_string(),
                    json!(create_args["workspace_id"].as_str().unwrap()),
                ),
                ("limit".to_string(), json!(10)),
            ]),
            fake_gh.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(refreshed["succeeded"], 3);
        assert_eq!(refreshed["failed"], 1);
        assert_eq!(refreshed["refreshed"].as_array().unwrap().len(), 4);
        let args = fs::read_to_string(args_file).unwrap();
        assert!(args.contains("pr\nview\nhttps://github.com/example/repo/pull/21\n--json"));
        assert!(args.contains("pr\nview\nhttps://github.com/example/repo/pull/22\n--json"));
        assert!(args.contains("pr\nview\nhttps://github.com/example/repo/pull/23\n--json"));

        let listed = list_pull_requests_tool(
            &state,
            &serde_json::Map::from_iter([(
                "workspace_id".to_string(),
                json!(create_args["workspace_id"].as_str().unwrap()),
            )]),
        )
        .unwrap();
        let pull_requests = listed["pull_requests"].as_array().unwrap();
        let open_records = pull_requests
            .iter()
            .filter(|pull_request| pull_request["branch"].as_str() == Some("codex/pr-batch-open"))
            .collect::<Vec<_>>();
        let merged = pull_requests
            .iter()
            .find(|pull_request| pull_request["branch"].as_str() == Some("codex/pr-batch-merged"))
            .unwrap();
        let failed = pull_requests
            .iter()
            .find(|pull_request| pull_request["branch"].as_str() == Some("codex/pr-batch-fails"))
            .unwrap();
        assert_eq!(open_records.len(), 2);
        for open in open_records {
            assert_eq!(open["status"], "open");
            assert_eq!(open["draft"], true);
            assert_eq!(open["number"], 21);
        }
        assert_eq!(merged["status"], "merged");
        assert_eq!(merged["merged"], true);
        assert_eq!(merged["number"], 22);
        assert_eq!(failed["status"], "refresh_failed");
        assert_eq!(failed["remote_state"], Value::Null);
        let raw_state = fs::read_to_string(state_dir.join("workspace_state.json")).unwrap();
        assert!(!raw_state.contains("secret pr body"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
        fs::remove_dir_all(fake_dir).unwrap();
    }

    #[test]
    fn refresh_pull_requests_caps_batch_size_and_marks_truncated() {
        let root = temp_project();
        let state_dir = temp_project();
        let fake_dir = temp_project();
        let fake_gh = fake_dir.join("gh");
        init_git_repo(&root);
        fs::write(
            &fake_gh,
            "#!/bin/sh\nprintf '%s\\n' '{\"state\":\"OPEN\",\"merged\":false,\"url\":\"https://github.com/example/repo/pull/99\",\"number\":99,\"title\":\"Limit PR\",\"baseRefName\":\"main\",\"headRefName\":\"codex/pr-limit\",\"isDraft\":false}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&fake_gh).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            fs::set_permissions(&fake_gh, permissions).unwrap();
        }

        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let create_args =
            serde_json::Map::from_iter([("workspace_id".to_string(), json!(workspace_id))]);
        for idx in 0..6 {
            let payload = json!({
                "branch": format!("codex/pr-limit-{idx}"),
                "base": "main",
                "title": format!("Limit {idx}"),
                "draft": false,
                "exit_code": 0,
                "success": true,
                "timed_out": false,
                "url": format!("https://github.com/example/repo/pull/limit-{idx}"),
                "body_chars": 10,
                "stdout": format!("https://github.com/example/repo/pull/limit-{idx}"),
                "stderr": "",
                "truncated": false
            });
            record_pull_request_from_tool_result(
                &state,
                Some("sid_pr_limit_refresh"),
                "create_pull_request",
                &create_args,
                &payload,
            );
            thread::sleep(Duration::from_millis(2));
        }

        let refreshed = refresh_pull_requests_tool_with_gh(
            &state,
            &serde_json::Map::from_iter([
                (
                    "workspace_id".to_string(),
                    json!(create_args["workspace_id"].as_str().unwrap()),
                ),
                ("limit".to_string(), json!(10)),
            ]),
            fake_gh.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            refreshed["refreshed"].as_array().unwrap().len(),
            MAX_PULL_REQUEST_REFRESHES
        );
        assert_eq!(refreshed["succeeded"], MAX_PULL_REQUEST_REFRESHES);
        assert_eq!(refreshed["truncated"], true);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
        fs::remove_dir_all(fake_dir).unwrap();
    }

    #[test]
    fn refresh_pull_request_status_can_match_handoff_by_url() {
        let root = temp_project();
        let state_dir = temp_project();
        let fake_dir = temp_project();
        let fake_gh = fake_dir.join("gh");
        init_git_repo(&root);
        fs::write(
            &fake_gh,
            "#!/bin/sh\nprintf '%s\\n' '{\"state\":\"OPEN\",\"merged\":false,\"url\":\"https://github.com/example/repo/pull/12\",\"number\":12,\"title\":\"URL Matched PR\",\"baseRefName\":\"main\",\"headRefName\":\"codex/pr-from-url\",\"isDraft\":true}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&fake_gh).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            fs::set_permissions(&fake_gh, permissions).unwrap();
        }

        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let create_args =
            serde_json::Map::from_iter([("workspace_id".to_string(), json!(workspace_id))]);
        let payload = json!({
            "branch": "codex/stale-local-branch",
            "base": "main",
            "title": "Initial PR",
            "draft": false,
            "exit_code": 0,
            "success": true,
            "timed_out": false,
            "url": "https://github.com/example/repo/pull/12",
            "body_chars": 17,
            "stdout": "https://github.com/example/repo/pull/12",
            "stderr": "",
            "truncated": false
        });
        record_pull_request_from_tool_result(
            &state,
            Some("sid_pr_url_refresh"),
            "create_pull_request",
            &create_args,
            &payload,
        );

        let refreshed = refresh_pull_request_status_tool_with_gh(
            &state,
            &serde_json::Map::from_iter([
                (
                    "workspace_id".to_string(),
                    json!(create_args["workspace_id"].as_str().unwrap()),
                ),
                (
                    "url".to_string(),
                    json!("https://github.com/example/repo/pull/12"),
                ),
            ]),
            fake_gh.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(refreshed["success"], true);
        assert_eq!(refreshed["pull_request"]["branch"], "codex/pr-from-url");
        assert_eq!(refreshed["pull_request"]["status"], "open");
        assert_eq!(refreshed["pull_request"]["draft"], true);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
        fs::remove_dir_all(fake_dir).unwrap();
    }

    #[test]
    fn refresh_pull_request_status_marks_handoff_failed_on_gh_error() {
        let root = temp_project();
        let state_dir = temp_project();
        let fake_dir = temp_project();
        let fake_gh = fake_dir.join("gh");
        init_git_repo(&root);
        Command::new("git")
            .args(["checkout", "-b", "codex/pr-refresh-fails"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        fs::write(&fake_gh, "#!/bin/sh\necho 'not found' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&fake_gh).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            fs::set_permissions(&fake_gh, permissions).unwrap();
        }

        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap().to_string();
        let create_args =
            serde_json::Map::from_iter([("workspace_id".to_string(), json!(workspace_id))]);
        let payload = json!({
            "branch": "codex/pr-refresh-fails",
            "base": "main",
            "title": "Refresh Fails",
            "draft": false,
            "exit_code": 0,
            "success": true,
            "timed_out": false,
            "url": "https://github.com/example/repo/pull/13",
            "body_chars": 12,
            "stdout": "https://github.com/example/repo/pull/13",
            "stderr": "",
            "truncated": false
        });
        record_pull_request_from_tool_result(
            &state,
            Some("sid_pr_refresh_fails"),
            "create_pull_request",
            &create_args,
            &payload,
        );
        update_persisted_pull_request(
            &state,
            create_args["workspace_id"].as_str().unwrap(),
            Some("codex/pr-refresh-fails"),
            None,
            |record| {
                record.remote_state = Some("open".to_string());
                record.merged = Some(false);
                record.number = Some(13);
                Ok(())
            },
        )
        .unwrap();

        let refreshed = refresh_pull_request_status_tool_with_gh(
            &state,
            &serde_json::Map::from_iter([(
                "workspace_id".to_string(),
                json!(create_args["workspace_id"].as_str().unwrap()),
            )]),
            fake_gh.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(refreshed["success"], false);
        assert_eq!(refreshed["pull_request"]["status"], "refresh_failed");
        assert_eq!(refreshed["pull_request"]["exit_code"], 1);
        assert_eq!(refreshed["pull_request"]["remote_state"], Value::Null);
        assert_eq!(refreshed["pull_request"]["merged"], Value::Null);
        assert_eq!(refreshed["pull_request"]["number"], Value::Null);
        let listed = list_pull_requests_tool(
            &state,
            &serde_json::Map::from_iter([(
                "workspace_id".to_string(),
                json!(create_args["workspace_id"].as_str().unwrap()),
            )]),
        )
        .unwrap();
        assert_eq!(listed["pull_requests"][0]["status"], "refresh_failed");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
        fs::remove_dir_all(fake_dir).unwrap();
    }

    #[test]
    fn audit_redacts_shell_command_and_output() {
        let root = temp_project();
        let state_dir = temp_project();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_shell"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let _ = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"shell","arguments":{"workspace_id":workspace_id,"command":"printf secret-output"}}),
            Some("sid_shell"),
        );
        let raw_audit = fs::read_to_string(state_dir.join("audit.jsonl")).unwrap();
        assert!(raw_audit.contains("\"tool\":\"shell\""));
        assert!(raw_audit.contains("\"command\":\"<redacted>\""));
        assert!(!raw_audit.contains("secret-output"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn open_worktree_creates_managed_workspace_in_execute_mode() {
        let root = temp_project();
        let state_dir = temp_project();
        init_git_repo(&root);
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let worktree = open_worktree_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("branch".to_string(), json!("codex/test-worktree")),
                ("task_id".to_string(), json!("ISSUE-123")),
                ("task".to_string(), json!("Verify task metadata")),
            ]),
        )
        .unwrap();
        let worktree_workspace_id = worktree["workspace_id"].as_str().unwrap();
        assert_eq!(worktree["branch"], "codex/test-worktree");
        assert_eq!(worktree["task_id"], "ISSUE-123");
        assert_eq!(worktree["task"], "Verify task metadata");
        let read = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(worktree_workspace_id)),
                ("path".to_string(), json!("README.md")),
            ]),
        )
        .unwrap();
        assert_eq!(read["content"], "# Test\n");
        let worktrees = managed_worktrees(&state_dir).unwrap();
        assert_eq!(worktrees.len(), 1);
        let managed_name = worktrees[0].file_name().unwrap().to_str().unwrap();
        let metadata = read_worktree_metadata(&state_dir, managed_name).unwrap();
        assert_eq!(metadata.task_id.as_deref(), Some("ISSUE-123"));
        assert_eq!(metadata.task.as_deref(), Some("Verify task metadata"));
        remove_managed_worktree(&worktrees[0]).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn list_worktrees_returns_managed_worktree_workspace_ids() {
        let root = temp_project();
        let state_dir = temp_project();
        init_git_repo(&root);
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let _ = open_worktree_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("branch".to_string(), json!("codex/list-worktree")),
                ("task_id".to_string(), json!("TASK-9")),
                ("task".to_string(), json!("List metadata")),
            ]),
        )
        .unwrap();
        let listed = list_worktrees_tool(&state).unwrap();
        let worktrees = listed["worktrees"].as_array().unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0]["available"], true);
        assert_eq!(worktrees[0]["branch"], "codex/list-worktree");
        assert_eq!(worktrees[0]["task_id"], "TASK-9");
        assert_eq!(worktrees[0]["task"], "List metadata");
        assert!(worktrees[0].get("root").is_none());
        let worktree_workspace_id = worktrees[0]["workspace_id"].as_str().unwrap();
        let read = read_file_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(worktree_workspace_id)),
                ("path".to_string(), json!("README.md")),
            ]),
        )
        .unwrap();
        assert_eq!(read["content"], "# Test\n");
        for worktree in managed_worktrees(&state_dir).unwrap() {
            remove_managed_worktree(&worktree).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn open_worktree_rejects_invalid_inputs() {
        let root = temp_project();
        let state_dir = temp_project();
        init_git_repo(&root);
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        let bad_branch = open_worktree_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("branch".to_string(), json!("-bad")),
            ]),
        )
        .unwrap_err();
        assert!(bad_branch.to_string().contains("may not start"));
        let bad_ref = open_worktree_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("base_ref".to_string(), json!("does-not-exist")),
            ]),
        )
        .unwrap_err();
        assert!(bad_ref.to_string().contains("base_ref"));
        let subdir = root.join("subdir");
        fs::create_dir(&subdir).unwrap();
        let sub_opened = open_workspace(&state, subdir.to_str().unwrap()).unwrap();
        let sub_workspace_id = sub_opened["workspace_id"].as_str().unwrap();
        let not_repo_root = open_worktree_tool(
            &state,
            &serde_json::Map::from_iter([("workspace_id".to_string(), json!(sub_workspace_id))]),
        )
        .unwrap_err();
        assert!(not_repo_root.to_string().contains("repository root"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn show_changes_summarizes_git_diff_and_recent_actions() {
        let root = temp_project();
        let state_dir = temp_project();
        init_git_repo(&root);
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        record_session_initialized(&state, "sid_changes");

        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_changes"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let _ = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"write","arguments":{"workspace_id":workspace_id,"path":"README.md","content":"# Changed\n"}}),
            Some("sid_changes"),
        );
        let _ = handle_tools_call(
            &state,
            Some(json!(3)),
            json!({"name":"move_path","arguments":{"workspace_id":workspace_id,"from_path":"README.md","to_path":"README-renamed.md"}}),
            Some("sid_changes"),
        );
        let changes = handle_tools_call(
            &state,
            Some(json!(4)),
            json!({"name":"show_changes","arguments":{"workspace_id":workspace_id}}),
            Some("sid_changes"),
        );
        let payload = &changes["result"]["structuredContent"];
        assert_eq!(changes["result"]["isError"], false);
        assert_eq!(payload["workspace_id"], workspace_id);
        assert!(payload["status"].as_str().unwrap().contains("README"));
        assert!(payload["stat"].as_str().unwrap().contains("README"));
        assert_eq!(payload["recent_actions"].as_array().unwrap().len(), 2);
        assert_eq!(payload["recent_actions"][0]["tool"], "write");
        assert_eq!(payload["recent_actions"][0]["path"], "README.md");
        assert_eq!(payload["recent_actions"][1]["tool"], "move_path");
        assert_eq!(payload["recent_actions"][1]["from_path"], "README.md");
        assert_eq!(payload["recent_actions"][1]["to_path"], "README-renamed.md");
        let rendered = handle_tools_call(
            &state,
            Some(json!(5)),
            json!({"name":"render_changes","arguments":{"workspace_id":workspace_id}}),
            Some("sid_changes"),
        );
        assert_eq!(
            rendered["result"]["structuredContent"]["workspace_id"],
            workspace_id
        );
        assert_eq!(
            rendered["result"]["_meta"]["openai/outputTemplate"],
            CHANGES_WIDGET_URI
        );
        assert_eq!(
            rendered["result"]["_meta"]["ui"]["resourceUri"],
            CHANGES_WIDGET_URI
        );
        let raw_state = fs::read_to_string(state_dir.join("workspace_state.json")).unwrap();
        assert!(!raw_state.contains("# Changed"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn initialized_sessions_are_capped() {
        let sessions = Mutex::new(InitializedSessions::default());
        for idx in 0..(MAX_INITIALIZED_SESSIONS + 5) {
            register_session(sessions.lock().unwrap(), format!("sid_{idx}"));
        }
        let sessions = sessions.lock().unwrap();
        assert_eq!(sessions.set.len(), MAX_INITIALIZED_SESSIONS);
        assert!(!sessions.set.contains("sid_0"));
        assert!(sessions
            .set
            .contains(&format!("sid_{}", MAX_INITIALIZED_SESSIONS + 4)));
    }

    #[test]
    fn git_status_on_non_git_workspace_is_tool_error() {
        let root = temp_project();
        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_git"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let status = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"git_status","arguments":{"workspace_id":workspace_id}}),
            Some("sid_git"),
        );
        assert_eq!(status["result"]["isError"], true);
        assert!(status["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("git command failed"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_diff_truncates_large_output() {
        let root = temp_project();
        Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&root)
            .status()
            .unwrap();
        fs::write(root.join("large.txt"), "base\n").unwrap();
        Command::new("git")
            .args(["add", "large.txt"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        let large = (0..12_000)
            .map(|idx| format!("changed line {idx}\n"))
            .collect::<String>();
        fs::write(root.join("large.txt"), large).unwrap();

        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_git"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let diff = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"git_diff","arguments":{"workspace_id":workspace_id}}),
            Some("sid_git"),
        );
        assert_eq!(diff["result"]["isError"], false);
        assert_eq!(diff["result"]["structuredContent"]["truncated"], true);
        assert!(
            diff["result"]["structuredContent"]["diff"]
                .as_str()
                .unwrap()
                .len()
                <= MAX_READ_BYTES
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn http_flow_opens_and_reads_workspace() {
        let root = temp_project();
        let state_dir = temp_project();
        fs::write(root.join("README.md"), "# Test\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let app = build_router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let (_, ping) =
            post_json(addr, json!({"jsonrpc":"2.0","id":1,"method":"ping"}), None).await;
        assert_eq!(ping["result"], json!({}));

        let (init_response, init) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            None,
        )
        .await;
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(
            init["result"]["capabilities"]["resources"]["listChanged"],
            false
        );
        let session_id = header_value(&init_response, "Mcp-Session-Id").unwrap();

        let (_, listed) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
            Some(&session_id),
        )
        .await;
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 21);
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "render_changes"
            && tool["_meta"]["openai/outputTemplate"] == CHANGES_WIDGET_URI));
        assert!(tools.iter().any(|tool| tool["name"] == "render_review"
            && tool["_meta"]["openai/outputTemplate"] == REVIEW_WIDGET_URI));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "render_pull_requests"
                && tool["_meta"]["openai/outputTemplate"] == PULL_REQUESTS_WIDGET_URI));
        assert!(tools.iter().any(|tool| tool["name"] == "render_edit_plans"
            && tool["_meta"]["openai/outputTemplate"] == EDIT_PLANS_WIDGET_URI));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "preview_patch"
                && tool["annotations"]["readOnlyHint"] == true));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "list_edit_plans"
                && tool["annotations"]["readOnlyHint"] == true));

        let (_, resources) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}),
            Some(&session_id),
        )
        .await;
        assert_eq!(
            resources["result"]["resources"][0]["uri"],
            CHANGES_WIDGET_URI
        );
        assert_eq!(
            resources["result"]["resources"][1]["uri"],
            REVIEW_WIDGET_URI
        );
        assert_eq!(
            resources["result"]["resources"][2]["uri"],
            PULL_REQUESTS_WIDGET_URI
        );
        assert_eq!(
            resources["result"]["resources"][3]["uri"],
            EDIT_PLANS_WIDGET_URI
        );
        assert_eq!(
            resources["result"]["resources"][0]["_meta"]["openai/widgetDescription"],
            "A compact change summary card for Codex workspace diffs and recent actions."
        );
        assert!(resources["result"]["resources"][2]["_meta"]["ui"]["csp"]
            .as_object()
            .unwrap()
            .contains_key("connectDomains"));
        assert!(resources["result"]["resources"][2]["_meta"]["ui"]["csp"]
            .as_object()
            .unwrap()
            .contains_key("resourceDomains"));
        assert!(resources["result"]["resources"][2]["_meta"]
            .as_object()
            .unwrap()
            .contains_key("openai/widgetCSP"));

        let (_, resource) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":CHANGES_WIDGET_URI}}),
            Some(&session_id),
        )
        .await;
        assert!(resource["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Workspace Changes"));

        let (_, review_resource) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":50,"method":"resources/read","params":{"uri":REVIEW_WIDGET_URI}}),
            Some(&session_id),
        )
        .await;
        assert!(review_resource["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Review Handoff"));

        let (_, pull_requests_resource) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":51,"method":"resources/read","params":{"uri":PULL_REQUESTS_WIDGET_URI}}),
            Some(&session_id),
        )
        .await;
        assert!(pull_requests_resource["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Pull Request Handoff"));

        let (_, edit_plans_resource) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":52,"method":"resources/read","params":{"uri":EDIT_PLANS_WIDGET_URI}}),
            Some(&session_id),
        )
        .await;
        assert!(edit_plans_resource["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Edit Plan History"));

        let (_, opened) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"open_workspace","arguments":{"path":root}}}),
            Some(&session_id),
        )
        .await;
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(opened["result"]["structuredContent"].get("root").is_none());

        let (_, read) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read","arguments":{"workspace_id":workspace_id,"path":"README.md"}}}),
            Some(&session_id),
        )
        .await;
        assert_eq!(read["result"]["structuredContent"]["content"], "# Test\n");

        let (_, session) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"show_session","arguments":{}}}),
            Some(&session_id),
        )
        .await;
        assert_eq!(
            session["result"]["structuredContent"]["session_id"],
            session_id
        );
        assert!(
            session["result"]["structuredContent"]["call_count"]
                .as_u64()
                .unwrap()
                >= 2
        );

        server.abort();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[tokio::test]
    async fn http_security_gates_reject_bad_requests() {
        let root = temp_project();
        let config = validate_raw(raw_config(root.clone())).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let app = build_router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string();
        let unauthorized = raw_http(
            addr,
            format!(
                "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
        .await;
        assert_eq!(http_status(&unauthorized), 401);

        let forbidden = raw_http(
            addr,
            format!(
                "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nOrigin: https://evil.example\r\nContent-Type: application/json\r\nAuthorization: Bearer secret\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
        .await;
        assert_eq!(http_status(&forbidden), 403);

        let unsupported = raw_http(
            addr,
            format!(
                "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: text/plain\r\nAuthorization: Bearer secret\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
        .await;
        assert_eq!(http_status(&unsupported), 415);

        let (_, denied) = post_json(
            addr,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            None,
        )
        .await;
        assert_eq!(denied["error"]["code"], -32002);

        server.abort();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn oauth_flow_issues_bearer_token_for_mcp() {
        let root = temp_project();
        let state_dir = temp_project();
        fs::write(root.join("README.md"), "# Test\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.public_base_url = Some("https://bridge.example".to_string());
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let owner_password = state
            .oauth
            .as_ref()
            .unwrap()
            .owner_secret
            .owner_password
            .clone();
        let app = build_router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let now = unix_ms();
        state
            .oauth
            .as_ref()
            .unwrap()
            .tokens
            .lock()
            .unwrap()
            .tokens
            .push(StoredOAuthToken {
                access_token: "legacy-empty-scope".to_string(),
                refresh_token: "legacy-refresh".to_string(),
                client_id: "legacy-client".to_string(),
                scope: "".to_string(),
                resource: Some("https://bridge.example/mcp".to_string()),
                issued_at_unix_ms: now,
                expires_at_unix_ms: now + OAUTH_ACCESS_TOKEN_TTL_MS,
                refresh_expires_at_unix_ms: now + OAUTH_REFRESH_TOKEN_TTL_MS,
            });

        let legacy_init_body =
            json!({"jsonrpc":"2.0","id":90,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})
                .to_string();
        let legacy_init = raw_http(
            addr,
            format!(
                "POST /mcp HTTP/1.1\r\nHost: bridge.example\r\nContent-Type: application/json\r\nAuthorization: Bearer legacy-empty-scope\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                legacy_init_body.len(),
                legacy_init_body
            ),
        )
        .await;
        assert_eq!(http_status(&legacy_init), 200);
        let legacy_session_id = header_value(&legacy_init, "Mcp-Session-Id").unwrap();
        let legacy_review_body =
            json!({"jsonrpc":"2.0","id":91,"method":"tools/call","params":{"name":"show_review","arguments":{"workspace_id":"workspace-any"}}})
                .to_string();
        let legacy_review = raw_http(
            addr,
            format!(
                "POST /mcp HTTP/1.1\r\nHost: bridge.example\r\nContent-Type: application/json\r\nAuthorization: Bearer legacy-empty-scope\r\nMcp-Session-Id: {legacy_session_id}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                legacy_review_body.len(),
                legacy_review_body
            ),
        )
        .await;
        let legacy_review_json: Value = serde_json::from_str(http_body(&legacy_review)).unwrap();
        assert_eq!(legacy_review_json["error"]["code"], -32003);
        assert!(legacy_review_json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("workspace:read"));
        for tool in [
            "list_pull_requests",
            "show_pull_requests",
            "render_pull_requests",
            "list_edit_plans",
            "show_edit_plans",
            "render_edit_plans",
        ] {
            let body = json!({"jsonrpc":"2.0","id":92,"method":"tools/call","params":{"name":tool,"arguments":{}}})
                .to_string();
            let response = raw_http(
                addr,
                format!(
                    "POST /mcp HTTP/1.1\r\nHost: bridge.example\r\nContent-Type: application/json\r\nAuthorization: Bearer legacy-empty-scope\r\nMcp-Session-Id: {legacy_session_id}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            )
            .await;
            let response_json: Value = serde_json::from_str(http_body(&response)).unwrap();
            assert_eq!(response_json["error"]["code"], -32003, "{tool}");
            assert!(response_json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("workspace:read"));
        }

        let metadata = get_raw(addr, PROTECTED_RESOURCE_METADATA_ENDPOINT, "bridge.example").await;
        assert_eq!(http_status(&metadata), 200);
        let metadata_body: Value = serde_json::from_str(http_body(&metadata)).unwrap();
        assert_eq!(metadata_body["resource"], "https://bridge.example/mcp");
        assert_eq!(
            metadata_body["authorization_servers"][0],
            "https://bridge.example"
        );

        let forbidden = get_raw(addr, PROTECTED_RESOURCE_METADATA_ENDPOINT, "evil.example").await;
        assert_eq!(http_status(&forbidden), 403);

        let verifier = "test-verifier-123";
        let challenge = s256(verifier);
        let unsupported_scope = post_form(
            addr,
            OAUTH_APPROVE_ENDPOINT,
            "bridge.example",
            &[
                ("response_type", "code"),
                ("client_id", "chatgpt-test"),
                ("redirect_uri", "https://chat.openai.com/aip/g/callback"),
                ("scope", "admin:everything"),
                ("resource", "https://bridge.example/mcp"),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("owner_password", &owner_password),
            ],
        )
        .await;
        assert_eq!(http_status(&unsupported_scope), 400);
        assert!(http_body(&unsupported_scope).contains("unsupported scope"));

        let approve = post_form(
            addr,
            OAUTH_APPROVE_ENDPOINT,
            "bridge.example",
            &[
                ("response_type", "code"),
                ("client_id", "chatgpt-test"),
                ("redirect_uri", "https://chat.openai.com/aip/g/callback"),
                ("scope", "workspace:read"),
                ("state", "state-1"),
                ("resource", "https://bridge.example/mcp"),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("owner_password", &owner_password),
            ],
        )
        .await;
        assert_eq!(http_status(&approve), 302);
        let location = header_value(&approve, "Location").unwrap();
        let redirect = url::Url::parse(&location).unwrap();
        let code = redirect
            .query_pairs()
            .find(|(key, _)| key == "code")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(
            redirect
                .query_pairs()
                .find(|(key, _)| key == "state")
                .unwrap()
                .1,
            "state-1"
        );

        let token_response = post_form(
            addr,
            OAUTH_TOKEN_ENDPOINT,
            "bridge.example",
            &[
                ("grant_type", "authorization_code"),
                ("client_id", "chatgpt-test"),
                ("redirect_uri", "https://chat.openai.com/aip/g/callback"),
                ("code", &code),
                ("code_verifier", verifier),
            ],
        )
        .await;
        assert_eq!(http_status(&token_response), 200);
        let token_body: Value = serde_json::from_str(http_body(&token_response)).unwrap();
        assert_eq!(token_body["token_type"], "Bearer");
        let access_token = token_body["access_token"].as_str().unwrap();
        let refresh_token = token_body["refresh_token"].as_str().unwrap();

        let init_body =
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})
                .to_string();
        let init = raw_http(
            addr,
            format!(
                "POST /mcp HTTP/1.1\r\nHost: bridge.example\r\nContent-Type: application/json\r\nAuthorization: Bearer {access_token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                init_body.len(),
                init_body
            ),
        )
        .await;
        assert_eq!(http_status(&init), 200);
        let init_json: Value = serde_json::from_str(http_body(&init)).unwrap();
        assert_eq!(
            init_json["result"]["serverInfo"]["name"],
            "codex-web-bridge-connector-rs"
        );

        let refresh = post_form(
            addr,
            OAUTH_TOKEN_ENDPOINT,
            "bridge.example",
            &[
                ("grant_type", "refresh_token"),
                ("client_id", "chatgpt-test"),
                ("refresh_token", refresh_token),
            ],
        )
        .await;
        assert_eq!(http_status(&refresh), 200);
        let refresh_body: Value = serde_json::from_str(http_body(&refresh)).unwrap();
        assert_ne!(refresh_body["access_token"], token_body["access_token"]);

        let stored = fs::read_to_string(state_dir.join("oauth_tokens.json")).unwrap();
        assert!(stored.contains("chatgpt-test"));
        assert!(!root.join("oauth_tokens.json").exists());

        server.abort();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[tokio::test]
    async fn oauth_read_scope_cannot_call_execute_tools() {
        let root = temp_project();
        let state_dir = temp_project();
        init_git_repo(&root);
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.public_base_url = Some("https://bridge.example".to_string());
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let app = build_router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let now = unix_ms();
        state
            .oauth
            .as_ref()
            .unwrap()
            .tokens
            .lock()
            .unwrap()
            .tokens
            .push(StoredOAuthToken {
                access_token: "read-only-token".to_string(),
                refresh_token: "read-only-refresh".to_string(),
                client_id: "read-only-client".to_string(),
                scope: "workspace:read".to_string(),
                resource: Some("https://bridge.example/mcp".to_string()),
                issued_at_unix_ms: now,
                expires_at_unix_ms: now + OAUTH_ACCESS_TOKEN_TTL_MS,
                refresh_expires_at_unix_ms: now + OAUTH_REFRESH_TOKEN_TTL_MS,
            });
        let init_body =
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})
                .to_string();
        let init = raw_http(
            addr,
            format!(
                "POST /mcp HTTP/1.1\r\nHost: bridge.example\r\nContent-Type: application/json\r\nAuthorization: Bearer read-only-token\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                init_body.len(),
                init_body
            ),
        )
        .await;
        assert_eq!(http_status(&init), 200);
        let session_id = header_value(&init, "Mcp-Session-Id").unwrap();
        for (tool, required_scope) in [
            ("refresh_pull_requests", "workspace:write"),
            ("shell", "shell"),
        ] {
            let body = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":{"workspace_id":"workspace-any","command":"pwd"}}})
                .to_string();
            let response = raw_http(
                addr,
                format!(
                    "POST /mcp HTTP/1.1\r\nHost: bridge.example\r\nContent-Type: application/json\r\nAuthorization: Bearer read-only-token\r\nMcp-Session-Id: {session_id}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            )
            .await;
            let response_json: Value = serde_json::from_str(http_body(&response)).unwrap();
            assert_eq!(response_json["error"]["code"], -32003, "{tool}");
            assert!(response_json["error"]["message"]
                .as_str()
                .unwrap()
                .contains(required_scope));
        }

        server.abort();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn audit_log_records_tool_calls_without_file_content() {
        let root = temp_project();
        let state_dir = temp_project();
        fs::write(root.join("README.md"), "secret body").unwrap();
        let mut raw = raw_config(root.clone());
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(WorkspaceRegistry::default())),
            initialized_sessions: Arc::new(Mutex::new(InitializedSessions::default())),
            oauth: None,
            persisted_state: None,
        };
        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_test"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let _ = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"read","arguments":{"workspace_id":workspace_id,"path":"README.md"}}),
            Some("sid_test"),
        );
        let raw_audit = fs::read_to_string(state_dir.join("audit.jsonl")).unwrap();
        assert!(raw_audit.contains("\"tool\":\"open_workspace\""));
        assert!(raw_audit.contains("\"tool\":\"read\""));
        assert!(raw_audit.contains("\"content_chars\":11"));
        assert!(raw_audit.contains("\"session_id\":\"sid_test\""));
        assert!(!raw_audit.contains("secret body"));
        let events = read_audit_events(&state_dir, 50).unwrap();
        let detail = session_detail(&events, "sid_test");
        assert_eq!(detail["call_count"], 2);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn create_note_saves_review_artifact_without_body_in_audit_or_state() {
        let root = temp_project();
        let state_dir = temp_project();
        fs::write(root.join("README.md"), "# Test\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Review;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        record_session_initialized(&state, "sid_review");

        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_review"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let note = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"create_note","arguments":{"workspace_id":workspace_id,"title":"Check README","body":"secret review body","severity":"medium","path":"README.md"}}),
            Some("sid_review"),
        );
        assert_eq!(note["result"]["isError"], false);
        assert_eq!(note["result"]["structuredContent"]["title"], "Check README");
        assert_eq!(note["result"]["structuredContent"]["body_chars"], 18);
        assert_eq!(
            note["result"]["_meta"]["codex-web-bridge/tool"],
            "create_note"
        );

        let raw_notes = fs::read_to_string(state_dir.join("review-notes.jsonl")).unwrap();
        assert!(raw_notes.contains("secret review body"));
        assert!(raw_notes.contains("sid_review"));
        {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(state_dir.join("review-notes.jsonl"))
                .unwrap();
            writeln!(file, "not-json").unwrap();
        }
        let raw_audit = fs::read_to_string(state_dir.join("audit.jsonl")).unwrap();
        assert!(raw_audit.contains("\"tool\":\"create_note\""));
        assert!(raw_audit.contains("\"body\":\"<redacted>\""));
        assert!(!raw_audit.contains("secret review body"));
        let raw_state = fs::read_to_string(state_dir.join("workspace_state.json")).unwrap();
        assert!(raw_state.contains("create_note"));
        assert!(!raw_state.contains("secret review body"));

        let listed = handle_tools_call(
            &state,
            Some(json!(3)),
            json!({"name":"list_notes","arguments":{"workspace_id":workspace_id,"severity":"medium","path":"README.md"}}),
            Some("sid_review"),
        );
        assert_eq!(listed["result"]["isError"], false);
        let notes = listed["result"]["structuredContent"]["notes"]
            .as_array()
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["title"], "Check README");
        assert_eq!(notes[0]["body"], "secret review body");
        assert_eq!(notes[0]["session_id"], "sid_review");
        assert_eq!(
            listed["result"]["_meta"]["codex-web-bridge/summary"]["notes"],
            1
        );
        let raw_audit_after_list = fs::read_to_string(state_dir.join("audit.jsonl")).unwrap();
        assert!(raw_audit_after_list.contains("\"tool\":\"list_notes\""));
        assert!(!raw_audit_after_list.contains("\"result\":{\"notes\":["));
        assert!(!raw_audit_after_list.contains("secret review body"));

        let plan = handle_tools_call(
            &state,
            Some(json!(4)),
            json!({"name":"create_edit_plan","arguments":{"workspace_id":workspace_id,"title":"Plan README","intent":"secret plan intent","paths":["README.md"]}}),
            Some("sid_review"),
        );
        assert_eq!(plan["result"]["isError"], false);
        let review = handle_tools_call(
            &state,
            Some(json!(5)),
            json!({"name":"show_review","arguments":{"workspace_id":workspace_id,"limit":10}}),
            Some("sid_review"),
        );
        assert_eq!(review["result"]["isError"], false);
        assert_eq!(
            review["result"]["structuredContent"]["notes"][0]["body"],
            "secret review body"
        );
        assert_eq!(
            review["result"]["structuredContent"]["edit_plans"][0]["intent"],
            "secret plan intent"
        );
        assert_eq!(
            review["result"]["_meta"]["codex-web-bridge/summary"]["notes"],
            1
        );
        assert_eq!(
            review["result"]["_meta"]["codex-web-bridge/summary"]["edit_plans"],
            1
        );
        let rendered = handle_tools_call(
            &state,
            Some(json!(6)),
            json!({"name":"render_review","arguments":{"workspace_id":workspace_id,"limit":10}}),
            Some("sid_review"),
        );
        assert_eq!(rendered["result"]["isError"], false);
        assert_eq!(
            rendered["result"]["_meta"]["openai/outputTemplate"],
            REVIEW_WIDGET_URI
        );
        let raw_audit_after_review = fs::read_to_string(state_dir.join("audit.jsonl")).unwrap();
        assert!(raw_audit_after_review.contains("\"tool\":\"show_review\""));
        assert!(raw_audit_after_review.contains("\"tool\":\"render_review\""));
        assert!(!raw_audit_after_review.contains("secret plan intent"));
        assert!(!raw_audit_after_review.contains("secret review body"));

        let missing_workspace = handle_tools_call(
            &state,
            Some(json!(7)),
            json!({"name":"list_notes","arguments":{}}),
            Some("sid_review"),
        );
        assert_eq!(missing_workspace["result"]["isError"], true);
        assert!(missing_workspace["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("workspace_id"));

        let bad_path = handle_tools_call(
            &state,
            Some(json!(8)),
            json!({"name":"create_note","arguments":{"workspace_id":workspace_id,"title":"Bad","body":"body","path":"../README.md"}}),
            Some("sid_review"),
        );
        assert_eq!(bad_path["result"]["isError"], true);
        assert!(bad_path["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(".."));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn list_notes_rejects_no_auth_connector_access() {
        let root = temp_project();
        let state_dir = temp_project();
        let mut raw = raw_config(root.clone());
        raw.owner_token = None;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        let opened = open_workspace(&state, root.to_str().unwrap()).unwrap();
        let workspace_id = opened["workspace_id"].as_str().unwrap();
        append_review_note(
            &state_dir,
            &json!({
                "note_id": "note-no-auth",
                "ts_unix_ms": 1,
                "session_id": "sid",
                "workspace_id": workspace_id,
                "title": "No auth note",
                "severity": "info",
                "path": Value::Null,
                "body": "should not be exposed"
            }),
        )
        .unwrap();
        let err = list_notes_tool(
            &state,
            &serde_json::Map::from_iter([("workspace_id".to_string(), json!(workspace_id))]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("authenticated connector access"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn create_edit_plan_persists_plan_without_patch_body_in_audit_or_state() {
        let root = temp_project();
        let state_dir = temp_project();
        fs::write(root.join("README.md"), "one\ntwo\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Review;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        record_session_initialized(&state, "sid_plan");

        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_plan"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let patch = "\
--- a/README.md
+++ b/README.md
@@ -1,2 +1,2 @@
 one
-two
+TWO
";
        let plan = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"create_edit_plan","arguments":{"workspace_id":workspace_id,"title":"Update README","intent":"secret implementation intent","paths":["README.md"],"patch":patch}}),
            Some("sid_plan"),
        );
        assert_eq!(plan["result"]["isError"], false);
        let payload = &plan["result"]["structuredContent"];
        assert_eq!(payload["title"], "Update README");
        assert_eq!(payload["intent_chars"], 28);
        assert_eq!(payload["paths"].as_array().unwrap().len(), 1);
        assert_eq!(payload["files"][0]["path"], "README.md");
        assert_eq!(payload["files"][0]["operation"], "modify");
        assert_eq!(
            fs::read_to_string(root.join("README.md")).unwrap(),
            "one\ntwo\n"
        );

        let listed = list_edit_plans_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("limit".to_string(), json!(10)),
            ]),
        )
        .unwrap();
        assert_eq!(listed["edit_plans"].as_array().unwrap().len(), 1);
        assert_eq!(listed["edit_plans"][0]["title"], "Update README");
        assert_eq!(
            listed["edit_plans"][0]["intent"],
            "secret implementation intent"
        );
        let approved_plan = handle_tools_call(
            &state,
            Some(json!(31)),
            json!({"name":"create_edit_plan","arguments":{"workspace_id":workspace_id,"title":"Approved README","intent":"approved plan intent","paths":["README.md"]}}),
            Some("sid_plan"),
        );
        assert_eq!(approved_plan["result"]["isError"], false);
        let approved_plan_id = approved_plan["result"]["structuredContent"]["plan_id"]
            .as_str()
            .unwrap();
        let approved = update_edit_plan_status_tool(
            &state,
            &serde_json::Map::from_iter([
                ("plan_id".to_string(), json!(approved_plan_id)),
                ("status".to_string(), json!("approved")),
            ]),
        )
        .unwrap();
        assert_eq!(approved["status"], "approved");
        let shown = show_edit_plans_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("status".to_string(), json!("approved")),
                ("limit".to_string(), json!(10)),
            ]),
        )
        .unwrap();
        assert_eq!(shown["edit_plans"].as_array().unwrap().len(), 1);
        assert_eq!(shown["edit_plans"][0]["title"], "Approved README");
        assert_eq!(shown["status_counts"]["approved"], 1);
        let rendered = tool_result("render_edit_plans", shown, false);
        assert_eq!(
            rendered["_meta"]["openai/outputTemplate"],
            EDIT_PLANS_WIDGET_URI
        );

        let raw_state = fs::read_to_string(state_dir.join("workspace_state.json")).unwrap();
        assert!(raw_state.contains("Update README"));
        assert!(raw_state.contains("secret implementation intent"));
        assert!(!raw_state.contains("+TWO"));
        let raw_audit = fs::read_to_string(state_dir.join("audit.jsonl")).unwrap();
        assert!(raw_audit.contains("\"tool\":\"create_edit_plan\""));
        assert!(raw_audit.contains("\"intent\":\"<redacted>\""));
        assert!(raw_audit.contains("\"patch\":\"<redacted>\""));
        assert!(!raw_audit.contains("secret implementation intent"));
        assert!(!raw_audit.contains("+TWO"));

        let bad_path = handle_tools_call(
            &state,
            Some(json!(3)),
            json!({"name":"create_edit_plan","arguments":{"workspace_id":workspace_id,"title":"Bad","intent":"body","paths":["../README.md"]}}),
            Some("sid_plan"),
        );
        assert_eq!(bad_path["result"]["isError"], true);
        assert!(bad_path["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(".."));
        #[cfg(unix)]
        {
            let outside = temp_project();
            fs::write(outside.join("leak.txt"), "secret").unwrap();
            std::os::unix::fs::symlink(outside.join("leak.txt"), root.join("link.txt")).unwrap();
            let bad_symlink = handle_tools_call(
                &state,
                Some(json!(4)),
                json!({"name":"create_edit_plan","arguments":{"workspace_id":workspace_id,"title":"Bad Symlink","intent":"body","paths":["link.txt"]}}),
                Some("sid_plan"),
            );
            assert_eq!(bad_symlink["result"]["isError"], true);
            assert!(bad_symlink["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("symlink"));
            fs::remove_dir_all(outside).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn approved_edit_plan_can_be_marked_applied_by_apply_patch() {
        let root = temp_project();
        let state_dir = temp_project();
        fs::write(root.join("README.md"), "one\ntwo\n").unwrap();
        let mut raw = raw_config(root.clone());
        raw.trust_level = TrustLevel::Execute;
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        record_session_initialized(&state, "sid_apply_plan");

        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_apply_plan"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let patch = "\
--- a/README.md
+++ b/README.md
@@ -1,2 +1,2 @@
 one
-two
+TWO
";
        let plan = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"create_edit_plan","arguments":{"workspace_id":workspace_id,"title":"Apply README","intent":"apply this approved plan","paths":["README.md"],"patch":patch}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(plan["result"]["isError"], false);
        let plan_id = plan["result"]["structuredContent"]["plan_id"]
            .as_str()
            .unwrap()
            .to_string();
        let intent_only = handle_tools_call(
            &state,
            Some(json!(3)),
            json!({"name":"create_edit_plan","arguments":{"workspace_id":workspace_id,"title":"Intent Only","intent":"approve paths only","paths":["README.md"]}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(intent_only["result"]["isError"], false);
        let intent_only_plan_id = intent_only["result"]["structuredContent"]["plan_id"]
            .as_str()
            .unwrap()
            .to_string();

        let blocked = handle_tools_call(
            &state,
            Some(json!(4)),
            json!({"name":"apply_patch","arguments":{"workspace_id":workspace_id,"patch":patch,"plan_id":plan_id}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(blocked["result"]["isError"], true);
        assert!(blocked["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("approved"));

        let approved = handle_tools_call(
            &state,
            Some(json!(5)),
            json!({"name":"update_edit_plan_status","arguments":{"plan_id":plan_id,"status":"approved","status_note":"ready to apply"}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(approved["result"]["isError"], false);
        assert_eq!(
            approved["result"]["structuredContent"]["status"],
            "approved"
        );
        let approved_intent_only = handle_tools_call(
            &state,
            Some(json!(6)),
            json!({"name":"update_edit_plan_status","arguments":{"plan_id":intent_only_plan_id,"status":"approved"}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(approved_intent_only["result"]["isError"], false);
        let no_summary_apply = handle_tools_call(
            &state,
            Some(json!(7)),
            json!({"name":"apply_patch","arguments":{"workspace_id":workspace_id,"patch":patch,"plan_id":intent_only_plan_id}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(no_summary_apply["result"]["isError"], true);
        assert!(no_summary_apply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no validated patch summary"));
        let different_patch = "\
--- a/README.md
+++ b/README.md
@@ -1,2 +1,2 @@
 one
-two
+THREE
";
        let mismatch_apply = handle_tools_call(
            &state,
            Some(json!(8)),
            json!({"name":"apply_patch","arguments":{"workspace_id":workspace_id,"patch":different_patch,"plan_id":plan_id}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(mismatch_apply["result"]["isError"], true);
        assert!(mismatch_apply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("does not match"));

        let applied = handle_tools_call(
            &state,
            Some(json!(9)),
            json!({"name":"apply_patch","arguments":{"workspace_id":workspace_id,"patch":patch,"plan_id":plan_id}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(applied["result"]["isError"], false);
        assert_eq!(
            applied["result"]["structuredContent"]["plan_status"],
            "applied"
        );
        assert_eq!(
            fs::read_to_string(root.join("README.md")).unwrap(),
            "one\nTWO\n"
        );

        let listed = list_edit_plans_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("status".to_string(), json!("applied")),
                ("limit".to_string(), json!(10)),
            ]),
        )
        .unwrap();
        let listed_plans = listed["edit_plans"].as_array().unwrap();
        assert_eq!(listed_plans.len(), 1);
        let listed_plan = &listed_plans[0];
        assert_eq!(listed_plan["plan_id"], plan_id);
        assert_eq!(listed_plan["status"], "applied");
        assert_eq!(listed_plan["applied_session_id"], "sid_apply_plan");
        assert_eq!(listed_plan["applied_files"][0]["path"], "README.md");
        let shown = show_edit_plans_tool(
            &state,
            &serde_json::Map::from_iter([
                ("workspace_id".to_string(), json!(workspace_id)),
                ("status".to_string(), json!("applied")),
            ]),
        )
        .unwrap();
        assert_eq!(shown["status_counts"]["applied"], 1);
        let raw_audit = fs::read_to_string(state_dir.join("audit.jsonl")).unwrap();
        assert!(raw_audit.contains("\"tool\":\"update_edit_plan_status\""));
        assert!(raw_audit.contains("\"status_note\":\"<redacted>\""));
        assert!(!raw_audit.contains("ready to apply"));

        let rollback = handle_tools_call(
            &state,
            Some(json!(10)),
            json!({"name":"update_edit_plan_status","arguments":{"plan_id":plan_id,"status":"draft"}}),
            Some("sid_apply_plan"),
        );
        assert_eq!(rollback["result"]["isError"], true);
        assert!(rollback["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("applied"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn persisted_state_tracks_session_workspaces_and_calls_without_content() {
        let root = temp_project();
        let state_dir = temp_project();
        fs::write(root.join("README.md"), "secret body").unwrap();
        let mut raw = raw_config(root.clone());
        raw.state_dir = Some(state_dir.clone());
        let config = validate_raw(raw).unwrap();
        let state = build_state(config).unwrap();
        record_session_initialized(&state, "sid_state");

        let opened = handle_tools_call(
            &state,
            Some(json!(1)),
            json!({"name":"open_workspace","arguments":{"path":root}}),
            Some("sid_state"),
        );
        let workspace_id = opened["result"]["structuredContent"]["workspace_id"]
            .as_str()
            .unwrap()
            .to_string();
        let _ = handle_tools_call(
            &state,
            Some(json!(2)),
            json!({"name":"read","arguments":{"workspace_id":workspace_id,"path":"README.md"}}),
            Some("sid_state"),
        );

        let raw_state = fs::read_to_string(state_dir.join("workspace_state.json")).unwrap();
        let state_json: Value = serde_json::from_str(&raw_state).unwrap();
        let session = &state_json["sessions"][0];
        assert_eq!(session["session_id"], "sid_state");
        assert_eq!(session["workspaces"][0]["workspace_id"], workspace_id);
        assert_eq!(session["tool_calls"][0]["tool"], "open_workspace");
        assert_eq!(session["tool_calls"][1]["tool"], "read");
        assert_eq!(session["tool_calls"][1]["path"], "README.md");
        assert!(!raw_state.contains("secret body"));

        let detail = show_session_tool(
            &state,
            &serde_json::Map::from_iter([("session_id".to_string(), json!("sid_state"))]),
            None,
        )
        .unwrap();
        assert_eq!(detail["session_id"], "sid_state");
        assert_eq!(detail["workspace_count"], 1);
        assert_eq!(detail["call_count"], 2);
        assert_eq!(detail["workspaces"][0]["kind"], "workspace");
        assert_eq!(detail["calls"][1]["path"], "README.md");

        let summaries = persisted_session_summaries(&state_dir, 10).unwrap();
        assert_eq!(summaries[0]["session_id"], "sid_state");
        assert_eq!(summaries[0]["workspace_count"], 1);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(state_dir).unwrap();
    }
}
