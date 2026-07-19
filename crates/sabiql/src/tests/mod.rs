mod adapter_mysql;
mod adapter_postgres;
mod adapter_sqlite;
pub mod harness;

use clap::Parser;

use super::{Args, Command};

#[cfg(not(feature = "self-update"))]
use super::self_update_disabled_message;

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

#[cfg(feature = "self-update")]
mod sha256_digest {
    use crate::{parse_sha256_digest, release_archive_name};

    const DIGEST: &str = "3ceb307a4d3f790abd375a49d74df9d46b99855f76dc790625abe6000775f82e";

    #[test]
    fn parses_sha256sum_style_content() {
        let content = format!("{DIGEST}  sabiql-x86_64-unknown-linux-gnu.tar.gz\n");
        assert_eq!(parse_sha256_digest(&content, "x.sha256").unwrap(), DIGEST);
    }

    #[test]
    fn parses_bare_digest_and_normalizes_case() {
        let content = format!("{}\n", DIGEST.to_uppercase());
        assert_eq!(parse_sha256_digest(&content, "x.sha256").unwrap(), DIGEST);
    }

    #[test]
    fn rejects_empty_content() {
        assert!(parse_sha256_digest("  \n", "x.sha256").is_err());
    }

    #[test]
    fn rejects_non_hex_or_wrong_length_digest() {
        assert!(parse_sha256_digest("nothex", "x.sha256").is_err());
        assert!(parse_sha256_digest(&DIGEST[..40], "x.sha256").is_err());
        let bad = format!("{}zz", &DIGEST[..62]);
        assert!(parse_sha256_digest(&bad, "x.sha256").is_err());
    }

    #[test]
    fn archive_name_matches_cargo_dist_convention() {
        let name = release_archive_name();
        let target = self_update::get_target();
        let extension = if cfg!(target_os = "windows") {
            "zip"
        } else {
            "tar.gz"
        };
        assert_eq!(name, format!("sabiql-{target}.{extension}"));
    }
}
