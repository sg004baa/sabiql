use clap::Parser;
use color_eyre::eyre::Result;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    #[cfg(feature = "self-update")]
    /// Update sabiql to the latest compatible version
    Update,
    #[cfg(not(feature = "self-update"))]
    /// Self-update is disabled in this build
    #[command(hide = true)]
    Update,
}

#[tokio::main]
#[allow(
    clippy::print_stderr,
    reason = "CLI error output before TUI initialization"
)]
async fn main() -> Result<()> {
    let args = Args::parse();
    if matches!(args.command, Some(Command::Update)) {
        #[cfg(feature = "self-update")]
        {
            return run_update();
        }
        #[cfg(not(feature = "self-update"))]
        {
            eprintln!("{}", self_update_disabled_message());
            std::process::exit(1);
        }
    }

    let _ = sabiql::run(None).await?;
    Ok(())
}

#[cfg(feature = "self-update")]
#[allow(clippy::print_stdout, reason = "CLI subcommand output, TUI not active")]
fn run_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current}");
    println!("Checking for updates...");

    let status = self_update::backends::github::Update::configure()
        .repo_owner("riii111")
        .repo_name("sabiql")
        .bin_name("sabiql")
        .show_download_progress(true)
        .no_confirm(true)
        .current_version(current)
        .build()?
        .update()?;

    if status.updated() {
        println!("Updated successfully: v{} -> {}", current, status.version());
    } else {
        println!("Already up to date (v{current}).");
    }

    Ok(())
}

#[cfg(not(feature = "self-update"))]
fn self_update_disabled_message() -> String {
    format!(
        "Self-update is not available in this build (v{}).\n\
         If installed via Homebrew: brew upgrade sabiql\n\
         If installed via cargo:    cargo install sabiql",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_returns_none() {
        let args = Args::parse_from(["sabiql"]);
        assert!(args.command.is_none());
    }

    #[test]
    fn update_subcommand_is_recognized() {
        let args = Args::parse_from(["sabiql", "update"]);
        assert!(matches!(args.command, Some(Command::Update)));
    }

    #[test]
    #[cfg(not(feature = "self-update"))]
    fn disabled_message_contains_version_and_upgrade_guidance() {
        let msg = self_update_disabled_message();
        assert!(msg.contains(env!("CARGO_PKG_VERSION")));
        assert!(msg.contains("brew upgrade sabiql"));
        assert!(msg.contains("cargo install sabiql"));
    }
}
