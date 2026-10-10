#!/usr/bin/env bash
set -euo pipefail

for changed_file in "$@"; do
	case "$changed_file" in
	.github/workflows/ci.yml | scripts/sqlite_safe_mode_changes.sh | scripts/test_sqlite_safe_mode_changes.sh | Cargo.lock | Cargo.toml | rust-toolchain.toml | crates/app/Cargo.toml | crates/domain/Cargo.toml | crates/infra/Cargo.toml | crates/infra/adapters/registry.rs | crates/infra/adapters/test_support.rs | crates/infra/adapters/sqlite/* | crates/app/ports/outbound/db_operation_error.rs | crates/app/ports/outbound/sqlite_path_validator.rs | crates/app/model/connection/error.rs | crates/app/model/connection/error_state.rs | crates/app/update/connection/error.rs | crates/domain/connection/database_type.rs | crates/domain/connection/sqlite_path.rs | crates/ui/features/connections/error.rs)
		echo 'sqlite_safe_mode=true'
		exit 0
		;;
	esac
done

echo 'sqlite_safe_mode=false'
