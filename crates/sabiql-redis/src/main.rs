use clap::Parser;
use color_eyre::eyre::Result;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long)]
    read_only: bool,
    #[arg(default_value = "redis://127.0.0.1:6379/0")]
    dsn: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let _ = sabiql_redis::run(args.dsn, args.read_only).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_flag_defaults_to_false() {
        let args = Args::parse_from(["sabiql-redis", "redis://localhost"]);

        assert!(!args.read_only);
        assert_eq!(args.dsn, "redis://localhost");
    }

    #[test]
    fn read_only_flag_parses_true() {
        let args = Args::parse_from(["sabiql-redis", "--read-only", "redis://localhost"]);

        assert!(args.read_only);
        assert_eq!(args.dsn, "redis://localhost");
    }
}
