#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
classifier="$script_dir/sqlite_safe_mode_changes.sh"

assert_classification() {
	local expected=$1
	shift

	local actual
	actual=$(bash "$classifier" "$@")
	if [ "$actual" != "sqlite_safe_mode=$expected" ]; then
		printf 'expected %s, got %s for: %s\n' "$expected" "$actual" "$*" >&2
		exit 1
	fi
}

while IFS='|' read -r expected paths; do
	if [ "$expected" = expected ]; then
		continue
	fi

	IFS=',' read -r -a changed_files <<<"$paths"
	assert_classification "$expected" "${changed_files[@]}"
done <<'CASES'
expected|paths
true|crates/infra/adapters/sqlite/sqlite3/metadata.rs
true|crates/infra/adapters/test_support.rs
true|crates/app/ports/outbound/db_operation_error.rs
true|crates/app/ports/outbound/sqlite_path_validator.rs
true|crates/app/model/connection/error.rs
true|crates/app/update/connection/error.rs
true|crates/domain/connection/sqlite_path.rs
true|crates/ui/features/connections/error.rs
true|Cargo.lock
true|.github/workflows/ci.yml
true|README.md,crates/infra/adapters/sqlite/sqlite3/metadata.rs
false|README.md
false|crates/app/ports/outbound/clipboard.rs
false|crates/app/ports/outbound/renderer.rs
false|crates/app/ports/outbound/settings_store.rs
false|crates/app/update/connection/lifecycle.rs
false|crates/domain/connection/config.rs
false|crates/infra/adapters/postgres/adapter.rs
CASES
