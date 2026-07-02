pub trait SqlDialect: Send + Sync {
    fn build_explain_sql(&self, query: &str) -> Option<String>;
    fn build_explain_analyze_sql(&self, query: &str) -> Option<String>;
    fn build_update_sql(
        &self,
        schema: &str,
        table: &str,
        column: &str,
        new_value: &str,
        pk_pairs: &[(String, String)],
    ) -> String;
    fn build_bulk_delete_sql(
        &self,
        schema: &str,
        table: &str,
        pk_pairs_per_row: &[Vec<(String, String)>],
    ) -> String;
    /// SELECT the given columns for the rows identified by pk pairs.
    /// Empty pk_pairs_per_row => no WHERE clause (select all).
    fn build_select_sql(
        &self,
        schema: &str,
        table: &str,
        columns: &[String],
        pk_pairs_per_row: &[Vec<(String, String)>],
    ) -> String;
    /// Multi-row INSERT: INSERT INTO t (cols) VALUES (...), (...);
    fn build_insert_sql(
        &self,
        schema: &str,
        table: &str,
        columns: &[String],
        rows: &[Vec<String>],
    ) -> String;
    /// One UPDATE statement per row, joined by newlines.
    fn build_row_update_sql(
        &self,
        schema: &str,
        table: &str,
        columns: &[String],
        rows: &[Vec<String>],
        pk_columns: &[String],
    ) -> String;
}
