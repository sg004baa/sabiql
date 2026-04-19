use std::sync::atomic::{AtomicU8, Ordering};

use async_trait::async_trait;

use crate::app::ports::{
    DbOperationError, DdlGenerator, DsnBuilder, MetadataProvider, QueryExecutor, SqlDialect,
};
use crate::domain::connection::{ConnectionProfile, DatabaseType};
use crate::domain::{DatabaseMetadata, QueryResult, Table, TableSignature, WriteExecutionResult};

use super::mysql::MySqlAdapter;
use super::postgres::PostgresAdapter;

const DB_TYPE_POSTGRESQL: u8 = 0;
const DB_TYPE_MYSQL: u8 = 1;

/// Routes port trait calls to the appropriate database adapter based on
/// DSN scheme (for async traits) or an atomic active-type flag (for sync traits).
pub struct DispatchAdapter {
    postgres: PostgresAdapter,
    mysql: MySqlAdapter,
    active_type: AtomicU8,
}

impl DispatchAdapter {
    pub fn new() -> Self {
        Self {
            postgres: PostgresAdapter::new(),
            mysql: MySqlAdapter::new(),
            active_type: AtomicU8::new(DB_TYPE_POSTGRESQL),
        }
    }

    /// Switch the active database type for sync traits (`SqlDialect`, `DdlGenerator`).
    /// Async traits route automatically by DSN scheme.
    pub fn set_active_type(&self, db_type: DatabaseType) {
        let val = match db_type {
            DatabaseType::PostgreSQL => DB_TYPE_POSTGRESQL,
            DatabaseType::MySQL => DB_TYPE_MYSQL,
        };
        self.active_type.store(val, Ordering::Relaxed);
    }

    fn active_type(&self) -> DatabaseType {
        match self.active_type.load(Ordering::Relaxed) {
            DB_TYPE_MYSQL => DatabaseType::MySQL,
            _ => DatabaseType::PostgreSQL,
        }
    }

    fn is_mysql(dsn: &str) -> bool {
        dsn.starts_with("mysql://")
    }
}

impl Default for DispatchAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Async traits — route by DSN scheme
// ---------------------------------------------------------------------------

#[async_trait]
impl MetadataProvider for DispatchAdapter {
    async fn fetch_metadata(&self, dsn: &str) -> Result<DatabaseMetadata, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql.fetch_metadata(dsn).await
        } else {
            self.postgres.fetch_metadata(dsn).await
        }
    }

    async fn fetch_table_signatures(
        &self,
        dsn: &str,
    ) -> Result<Vec<TableSignature>, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql.fetch_table_signatures(dsn).await
        } else {
            self.postgres.fetch_table_signatures(dsn).await
        }
    }

    async fn fetch_table_detail(
        &self,
        dsn: &str,
        schema: &str,
        table: &str,
    ) -> Result<Table, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql.fetch_table_detail(dsn, schema, table).await
        } else {
            self.postgres.fetch_table_detail(dsn, schema, table).await
        }
    }

    async fn fetch_table_columns_and_fks(
        &self,
        dsn: &str,
        schema: &str,
        table: &str,
    ) -> Result<Table, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql
                .fetch_table_columns_and_fks(dsn, schema, table)
                .await
        } else {
            self.postgres
                .fetch_table_columns_and_fks(dsn, schema, table)
                .await
        }
    }
}

#[async_trait]
impl QueryExecutor for DispatchAdapter {
    async fn execute_preview(
        &self,
        dsn: &str,
        schema: &str,
        table: &str,
        limit: usize,
        offset: usize,
        read_only: bool,
    ) -> Result<QueryResult, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql
                .execute_preview(dsn, schema, table, limit, offset, read_only)
                .await
        } else {
            self.postgres
                .execute_preview(dsn, schema, table, limit, offset, read_only)
                .await
        }
    }

    async fn execute_adhoc(
        &self,
        dsn: &str,
        query: &str,
        read_only: bool,
    ) -> Result<QueryResult, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql.execute_adhoc(dsn, query, read_only).await
        } else {
            self.postgres.execute_adhoc(dsn, query, read_only).await
        }
    }

    async fn execute_write(
        &self,
        dsn: &str,
        query: &str,
        read_only: bool,
    ) -> Result<WriteExecutionResult, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql.execute_write(dsn, query, read_only).await
        } else {
            self.postgres.execute_write(dsn, query, read_only).await
        }
    }

    async fn count_query_rows(
        &self,
        dsn: &str,
        query: &str,
        read_only: bool,
    ) -> Result<usize, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql.count_query_rows(dsn, query, read_only).await
        } else {
            self.postgres.count_query_rows(dsn, query, read_only).await
        }
    }

    async fn export_to_csv(
        &self,
        dsn: &str,
        query: &str,
        path: &std::path::Path,
        read_only: bool,
    ) -> Result<usize, DbOperationError> {
        if Self::is_mysql(dsn) {
            self.mysql.export_to_csv(dsn, query, path, read_only).await
        } else {
            self.postgres
                .export_to_csv(dsn, query, path, read_only)
                .await
        }
    }
}

// ---------------------------------------------------------------------------
// Sync traits — route by atomic active type
// ---------------------------------------------------------------------------

impl DsnBuilder for DispatchAdapter {
    fn build_dsn(&self, profile: &ConnectionProfile) -> String {
        match profile.database_type {
            DatabaseType::MySQL => self.mysql.build_dsn(profile),
            DatabaseType::PostgreSQL => self.postgres.build_dsn(profile),
        }
    }
}

impl SqlDialect for DispatchAdapter {
    fn build_update_sql(
        &self,
        schema: &str,
        table: &str,
        column: &str,
        new_value: &str,
        pk_pairs: &[(String, String)],
    ) -> String {
        match self.active_type() {
            DatabaseType::MySQL => self
                .mysql
                .build_update_sql(schema, table, column, new_value, pk_pairs),
            DatabaseType::PostgreSQL => self
                .postgres
                .build_update_sql(schema, table, column, new_value, pk_pairs),
        }
    }

    fn build_bulk_delete_sql(
        &self,
        schema: &str,
        table: &str,
        pk_pairs_per_row: &[Vec<(String, String)>],
    ) -> String {
        match self.active_type() {
            DatabaseType::MySQL => {
                self.mysql
                    .build_bulk_delete_sql(schema, table, pk_pairs_per_row)
            }
            DatabaseType::PostgreSQL => {
                self.postgres
                    .build_bulk_delete_sql(schema, table, pk_pairs_per_row)
            }
        }
    }
}

impl DdlGenerator for DispatchAdapter {
    fn generate_ddl(&self, table: &Table) -> String {
        match self.active_type() {
            DatabaseType::MySQL => self.mysql.generate_ddl(table),
            DatabaseType::PostgreSQL => self.postgres.generate_ddl(table),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod routing {
        use super::*;

        #[test]
        fn is_mysql_detects_mysql_scheme() {
            assert!(DispatchAdapter::is_mysql("mysql://user:pass@host:3306/db"));
        }

        #[test]
        fn is_mysql_rejects_postgres_scheme() {
            assert!(!DispatchAdapter::is_mysql(
                "postgres://user:pass@host:5432/db"
            ));
        }

        #[test]
        fn is_mysql_rejects_service_dsn() {
            assert!(!DispatchAdapter::is_mysql("service=mydb"));
        }

        #[test]
        fn default_active_type_is_postgresql() {
            let adapter = DispatchAdapter::new();
            assert_eq!(adapter.active_type(), DatabaseType::PostgreSQL);
        }

        #[test]
        fn set_active_type_switches_to_mysql() {
            let adapter = DispatchAdapter::new();
            adapter.set_active_type(DatabaseType::MySQL);
            assert_eq!(adapter.active_type(), DatabaseType::MySQL);
        }

        #[test]
        fn set_active_type_switches_back_to_postgresql() {
            let adapter = DispatchAdapter::new();
            adapter.set_active_type(DatabaseType::MySQL);
            adapter.set_active_type(DatabaseType::PostgreSQL);
            assert_eq!(adapter.active_type(), DatabaseType::PostgreSQL);
        }
    }

    mod dsn_builder_dispatch {
        use super::*;
        use crate::app::ports::DsnBuilder;
        use crate::domain::connection::SslMode;

        #[test]
        fn routes_to_postgres_for_postgresql_profile() {
            let adapter = DispatchAdapter::new();
            let profile = ConnectionProfile::new(
                "Test",
                "localhost",
                5432,
                "testdb",
                "user",
                "pass",
                SslMode::Prefer,
                DatabaseType::PostgreSQL,
            )
            .unwrap();

            let dsn = adapter.build_dsn(&profile);
            assert!(dsn.starts_with("postgres://"));
        }

        #[test]
        fn routes_to_mysql_for_mysql_profile() {
            let adapter = DispatchAdapter::new();
            let profile = ConnectionProfile::new(
                "Test",
                "localhost",
                3306,
                "testdb",
                "user",
                "pass",
                SslMode::Prefer,
                DatabaseType::MySQL,
            )
            .unwrap();

            let dsn = adapter.build_dsn(&profile);
            assert!(dsn.starts_with("mysql://"));
        }
    }

    mod sync_trait_dispatch {
        use super::*;
        use crate::app::ports::{DdlGenerator, SqlDialect};
        use crate::domain::{Column, Table};

        fn make_table() -> Table {
            Table {
                schema: "public".to_string(),
                name: "users".to_string(),
                owner: None,
                columns: vec![Column {
                    name: "id".to_string(),
                    data_type: "int".to_string(),
                    nullable: false,
                    is_primary_key: true,
                    default: None,
                    is_unique: false,
                    comment: None,
                    ordinal_position: 1,
                }],
                primary_key: Some(vec!["id".to_string()]),
                foreign_keys: vec![],
                indexes: vec![],
                rls: None,
                triggers: vec![],
                row_count_estimate: None,
                comment: None,
            }
        }

        #[test]
        fn ddl_uses_double_quotes_for_postgresql() {
            let adapter = DispatchAdapter::new();
            let ddl = adapter.generate_ddl(&make_table());
            assert!(ddl.contains('"'), "PostgreSQL DDL should use double quotes");
        }

        #[test]
        fn ddl_uses_backticks_for_mysql() {
            let adapter = DispatchAdapter::new();
            adapter.set_active_type(DatabaseType::MySQL);
            let ddl = adapter.generate_ddl(&make_table());
            assert!(ddl.contains('`'), "MySQL DDL should use backticks");
        }

        #[test]
        fn update_sql_uses_double_quotes_for_postgresql() {
            let adapter = DispatchAdapter::new();
            let sql = adapter.build_update_sql(
                "public",
                "users",
                "name",
                "val",
                &[("id".into(), "1".into())],
            );
            assert!(sql.contains('"'));
        }

        #[test]
        fn update_sql_uses_backticks_for_mysql() {
            let adapter = DispatchAdapter::new();
            adapter.set_active_type(DatabaseType::MySQL);
            let sql = adapter.build_update_sql(
                "mydb",
                "users",
                "name",
                "val",
                &[("id".into(), "1".into())],
            );
            assert!(sql.contains('`'));
        }

        #[test]
        fn bulk_delete_routes_by_active_type() {
            let adapter = DispatchAdapter::new();
            let rows = vec![vec![("id".to_string(), "1".to_string())]];

            let pg_sql = adapter.build_bulk_delete_sql("public", "users", &rows);
            assert!(pg_sql.contains('"'));

            adapter.set_active_type(DatabaseType::MySQL);
            let my_sql = adapter.build_bulk_delete_sql("mydb", "users", &rows);
            assert!(my_sql.contains('`'));
        }
    }
}
