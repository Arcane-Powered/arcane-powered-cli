use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};

use arcane_cli::commands::{auth, builds, init, jobs, push, release, titles};
use arcane_cli::error::Result;
use arcane_cli::fastcdc::ChunkingMode;

#[derive(Parser)]
#[command(
    name = "arcane",
    version,
    about = "Official CLI for uploading and managing game builds on Arcane Powered",
    propagate_version = true
)]
struct Cli {
    /// Emit machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Save studio API key and portal URL to ~/.config/arcane/config.json
    Login {
        /// Studio API key (if omitted, prompts interactively)
        #[arg(long, env = "ARCANE_API_KEY")]
        api_key: Option<String>,
        /// Portal origin URL (default: https://portal.arcane-powered.com)
        #[arg(long, env = "ARCANE_PORTAL_URL")]
        portal_url: Option<String>,
        /// Skip verifying credentials against the portal
        #[arg(long)]
        no_verify: bool,
    },

    /// Remove saved credentials from ~/.config/arcane/config.json
    Logout,

    /// Show current authentication status and portal connection
    Whoami,

    /// Generate an arcane.json project configuration file
    Init {
        /// Default title ID to write into arcane.json
        #[arg(long)]
        title: Option<String>,
        /// Overwrite existing arcane.json
        #[arg(long)]
        force: bool,
    },

    /// List the studio's games and add-ons
    Titles,

    /// Upload and publish a game or add-on build
    Push {
        /// Folder containing the build to upload (optional if configured in arcane.json)
        folder: Option<PathBuf>,

        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,

        /// Target platform (windows, macos, linux, web, android)
        #[arg(long)]
        platform: String,

        /// Build version (defaults to exact git tag or GITHUB_REF_NAME)
        #[arg(long)]
        version: Option<String>,

        /// Keep the build as a draft instead of publishing it to players
        #[arg(long)]
        draft: bool,

        /// Publish at this ISO 8601 date (kept as draft until then)
        #[arg(long)]
        scheduled_at: Option<String>,

        /// Create an empty draft patch note linked to this build version
        #[arg(long)]
        create_notes: bool,

        /// Create a draft patch note pre-filled from a Markdown file (implies --create-notes)
        #[arg(long)]
        notes_file: Option<PathBuf>,

        /// Chunking mode for native builds (default: cdc)
        #[arg(long, value_enum)]
        chunking: Option<ChunkingArg>,

        /// Upload raw chunks without zstd compression
        #[arg(long)]
        no_compress: bool,

        /// Concurrent chunk upload workers (default: 6)
        #[arg(long)]
        concurrency: Option<usize>,

        /// Bypass the local mtime+size hash cache
        #[arg(long)]
        no_cache: bool,

        /// Skip files or folders matching this name or glob (repeatable)
        #[arg(long)]
        ignore: Vec<String>,

        /// Web only: entry HTML page (default: index.html)
        #[arg(long)]
        entry: Option<String>,

        /// Web only: fixed viewport size, e.g. 1280x720
        #[arg(long)]
        viewport: Option<String>,

        /// Web only: disable fullscreen
        #[arg(long)]
        no_fullscreen: bool,

        /// Web only: comma-separated https origins the game talks to
        #[arg(long)]
        origins: Option<String>,

        /// Web only: allow eval()
        #[arg(long)]
        unsafe_eval: bool,

        /// Web only: comma-separated inputs (keyboard, mouse, gamepad, touch)
        #[arg(long)]
        inputs: Option<String>,

        /// Web only: enable the Arcane web SDK bridge
        #[arg(long)]
        sdk_bridge: bool,
    },

    /// List recent build publish jobs for a title
    Jobs {
        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,
    },

    /// Inspect, pin, unpin, or delete builds for a title
    Builds {
        #[command(subcommand)]
        command: BuildsCommands,
    },

    /// Point a build channel (e.g. live, beta) to a specific version
    Release {
        /// Channel name (e.g. live, beta)
        channel: String,
        /// Build version to activate on the channel
        version: String,
        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,
        /// Restrict channel switch to a single platform
        #[arg(long)]
        platform: Option<String>,
    },

    /// Roll back the live channel to an earlier published version
    Rollback {
        /// Previously published version to restore
        version: String,
        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,
    },

    /// Generate shell completions (bash, zsh, fish, powershell)
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum BuildsCommands {
    /// List builds with channels and deduplicated CAS storage footprint
    List {
        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,
        /// Filter by platform
        #[arg(long)]
        platform: Option<String>,
    },
    /// Pin a build to protect it from deletion
    Pin {
        /// Build UUID to pin
        build_id: String,
        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,
    },
    /// Unpin a previously pinned build
    Unpin {
        /// Build UUID to unpin
        build_id: String,
        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,
    },
    /// Delete an inactive, unpinned build
    Delete {
        /// Build UUID to delete
        build_id: String,
        /// Title ID (optional if set in arcane.json)
        #[arg(long)]
        title: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ChunkingArg {
    Cdc,
    Fixed,
}

impl From<ChunkingArg> for ChunkingMode {
    fn from(value: ChunkingArg) -> Self {
        match value {
            ChunkingArg::Cdc => ChunkingMode::Cdc,
            ChunkingArg::Fixed => ChunkingMode::Fixed,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_mode = cli.json;

    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if json_mode {
                let payload = serde_json::json!({
                    "ok": false,
                    "error": err.to_string(),
                    "status": err.status(),
                });
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                eprintln!("error: {err}");
            }
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.command {
        Commands::Login {
            api_key,
            portal_url,
            no_verify,
        } => {
            auth::login(auth::LoginArgs {
                api_key,
                portal_url,
                no_verify,
                json,
            })
            .await
        }
        Commands::Logout => auth::logout(json),
        Commands::Whoami => auth::whoami(json).await,
        Commands::Init { title, force } => init::run(init::InitArgs { title, force, json }),
        Commands::Titles => titles::run(json).await,
        Commands::Push {
            folder,
            title,
            platform,
            version,
            draft,
            scheduled_at,
            create_notes,
            notes_file,
            chunking,
            no_compress,
            concurrency,
            no_cache,
            ignore,
            entry,
            viewport,
            no_fullscreen,
            origins,
            unsafe_eval,
            inputs,
            sdk_bridge,
        } => {
            push::run(push::PushArgs {
                folder,
                title,
                platform,
                version,
                draft,
                scheduled_at,
                create_notes,
                notes_file,
                chunking: chunking.map(Into::into),
                no_compress,
                concurrency,
                no_cache,
                ignore,
                entry,
                viewport,
                no_fullscreen,
                origins,
                unsafe_eval,
                inputs,
                sdk_bridge,
                json,
            })
            .await
        }
        Commands::Jobs { title } => jobs::run(jobs::JobsArgs { title, json }).await,
        Commands::Builds { command } => match command {
            BuildsCommands::List { title, platform } => {
                builds::list(builds::BuildsListArgs {
                    title,
                    platform,
                    json,
                })
                .await
            }
            BuildsCommands::Pin { build_id, title } => {
                builds::set_pinned(
                    builds::BuildActionArgs {
                        title,
                        build_id,
                        json,
                    },
                    true,
                )
                .await
            }
            BuildsCommands::Unpin { build_id, title } => {
                builds::set_pinned(
                    builds::BuildActionArgs {
                        title,
                        build_id,
                        json,
                    },
                    false,
                )
                .await
            }
            BuildsCommands::Delete { build_id, title } => {
                builds::delete(builds::BuildActionArgs {
                    title,
                    build_id,
                    json,
                })
                .await
            }
        },
        Commands::Release {
            channel,
            version,
            title,
            platform,
        } => {
            release::release(release::ReleaseArgs {
                channel,
                version,
                title,
                platform,
                json,
            })
            .await
        }
        Commands::Rollback { version, title } => {
            release::rollback(release::RollbackArgs {
                version,
                title,
                json,
            })
            .await
        }
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "arcane", &mut io::stdout());
            Ok(())
        }
    }
}
