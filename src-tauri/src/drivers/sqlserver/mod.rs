//! Microsoft SQL Server driver (built-in).

pub mod extract;
pub mod helpers;
pub mod introspection;
pub mod pool;
pub mod version;

use std::collections::HashMap;

use async_trait::async_trait;

use crate::drivers::driver_trait::{
    DatabaseDriver, DriverCapabilities, PluginManifest,
};
use crate::models::{
    ConnectionParams, DataTypeInfo, ForeignKey, Index, Pagination, QueryResult, RoutineInfo,
    RoutineParameter, TableColumn, TableInfo, TableSchema, ViewInfo,
};
use crate::pool_manager::get_sqlserver_pool;

/// Convert a `serde_json::Value` into a SQL Server literal for use in
/// simple (non-parameterized) queries. Strings are escaped by doubling
/// single quotes.
fn json_to_sql_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            format!("N'{}'", s.replace('\'', "''"))
        }
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            let json_str = serde_json::to_string(v).unwrap_or_default();
            format!("N'{}'", json_str.replace('\'', "''"))
        }
    }
}

/// Built-in SQL Server driver. Backed by `mssql-tiberius-bridge` + `deadpool`.
pub struct SqlServerDriver {
    manifest: PluginManifest,
}

impl SqlServerDriver {
    pub fn new() -> Self {
        Self {
            manifest: PluginManifest {
                id: "sqlserver".to_string(),
                name: "SQL Server".to_string(),
                version: "0.1.0".to_string(),
                description: "Microsoft SQL Server".to_string(),
                default_port: Some(1433),
                capabilities: DriverCapabilities {
                    schemas: true,
                    views: true,
                    routines: true,
                    file_based: false,
                    folder_based: false,
                    connection_string: true,
                    connection_string_example:
                        "server=localhost,1433;database=master;user id=sa;password=...;encrypt=true"
                            .into(),
                    identifier_quote: "\"".into(),
                    alter_primary_key: true,
                    auto_increment_keyword: "IDENTITY(1,1)".into(),
                    serial_type: String::new(),
                    inline_pk: false,
                    alter_column: false,
                    create_foreign_keys: false,
                    no_connection_required: false,
                    manage_tables: false,
                    readonly: false,
                },
                is_builtin: true,
                default_username: "sa".to_string(),
                color: "#cc2927".to_string(),
                icon: "database".to_string(),
                settings: vec![],
                ui_extensions: None,
            },
        }
    }
}

impl Default for SqlServerDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// Acquire a bridge client from the pool.
async fn acquire(
    params: &ConnectionParams,
) -> Result<deadpool::managed::Object<pool::BridgeManager>, String> {
    let pool = get_sqlserver_pool(params).await?;
    pool.get().await.map_err(|e| e.to_string())
}

#[async_trait]
impl DatabaseDriver for SqlServerDriver {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn get_data_types(&self) -> Vec<DataTypeInfo> {
        // Populated in Phase 1 Day 5+; empty vec keeps clipboard-import UI
        // inert for now (the readonly flag already hides editing surfaces).
        Vec::new()
    }

    fn map_inferred_type(&self, kind: &str) -> String {
        match kind {
            "TEXT" => "NVARCHAR(MAX)".into(),
            "INTEGER" => "INT".into(),
            "REAL" => "FLOAT".into(),
            "BOOLEAN" => "BIT".into(),
            "DATE" => "DATE".into(),
            "DATETIME" => "DATETIME2".into(),
            "JSON" => "NVARCHAR(MAX)".into(),
            other => other.into(),
        }
    }

    /// ADO.NET-style connection string. Not used internally (we use the pool),
    /// kept for UI preview + copy-to-clipboard.
    fn build_connection_url(&self, params: &ConnectionParams) -> Result<String, String> {
        let host = params.host.as_deref().unwrap_or("localhost");
        let port = params.port.unwrap_or(1433);
        let db = params.database.primary();
        let user = params.username.as_deref().unwrap_or("");
        let pass = params.password.as_deref().unwrap_or("");
        Ok(format!(
            "Server={host},{port};Database={db};User Id={user};Password={pass};TrustServerCertificate=true;Encrypt=true"
        ))
    }

    async fn test_connection(&self, params: &ConnectionParams) -> Result<(), String> {
        let mut conn = acquire(params).await?;
        conn.simple_query("SELECT 1")
            .await
            .map_err(|e| e.to_string())?
            .into_first_result();
        Ok(())
    }

    async fn get_databases(&self, params: &ConnectionParams) -> Result<Vec<String>, String> {
        let mut conn = acquire(params).await?;
        // Skip system DBs (database_id <= 4: master, tempdb, model, msdb)
        let rows = conn
            .simple_query(
                "SELECT name FROM sys.databases WHERE database_id > 4 ORDER BY name",
            )
            .await
            .map_err(|e| e.to_string())?
            .into_first_result();

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(name) = row.get::<&str, _>(0) {
                out.push(name.to_string());
            }
        }
        Ok(out)
    }

    async fn get_schemas(&self, params: &ConnectionParams) -> Result<Vec<String>, String> {
        let mut conn = acquire(params).await?;
        // User schemas: schema_id < 16384 excludes built-in (sys, INFORMATION_SCHEMA, guest, ...).
        // We also exclude the noise schemas explicitly; `dbo` is the default owner and must stay.
        let rows = conn
            .simple_query(
                "SELECT name FROM sys.schemas \
                 WHERE schema_id < 16384 \
                   AND name NOT IN ('sys','INFORMATION_SCHEMA','guest','db_owner','db_accessadmin','db_securityadmin','db_ddladmin','db_backupoperator','db_datareader','db_datawriter','db_denydatareader','db_denydatawriter') \
                 ORDER BY name",
            )
            .await
            .map_err(|e| e.to_string())?
            .into_first_result();

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(name) = row.get::<&str, _>(0) {
                out.push(name.to_string());
            }
        }
        Ok(out)
    }

    // --- Schema inspection (Day 4) -----------------------------------------

    async fn get_tables(
        &self,
        params: &ConnectionParams,
        schema: Option<&str>,
    ) -> Result<Vec<TableInfo>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_tables(&mut conn, schema.unwrap_or("dbo")).await
    }

    async fn get_columns(
        &self,
        params: &ConnectionParams,
        table: &str,
        schema: Option<&str>,
    ) -> Result<Vec<TableColumn>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_columns(&mut conn, table, schema).await
    }

    async fn get_foreign_keys(
        &self,
        params: &ConnectionParams,
        table: &str,
        schema: Option<&str>,
    ) -> Result<Vec<ForeignKey>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_foreign_keys(&mut conn, table, schema).await
    }

    async fn get_indexes(
        &self,
        params: &ConnectionParams,
        table: &str,
        schema: Option<&str>,
    ) -> Result<Vec<Index>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_indexes(&mut conn, table, schema).await
    }

    // --- Views --------------------------------------------------------------

    async fn get_views(
        &self,
        params: &ConnectionParams,
        schema: Option<&str>,
    ) -> Result<Vec<ViewInfo>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_views(&mut conn, schema.unwrap_or("dbo")).await
    }

    async fn get_view_definition(
        &self,
        params: &ConnectionParams,
        view_name: &str,
        schema: Option<&str>,
    ) -> Result<String, String> {
        let mut conn = acquire(params).await?;
        introspection::get_module_definition(&mut conn, view_name, schema).await
    }

    async fn get_view_columns(
        &self,
        params: &ConnectionParams,
        view_name: &str,
        schema: Option<&str>,
    ) -> Result<Vec<TableColumn>, String> {
        // `sys.columns` + `sys.types` work identically for views, so we reuse
        // the table introspection. The PK sub-query returns 0 for views
        // (no primary key on views), which is the correct behaviour.
        let mut conn = acquire(params).await?;
        introspection::get_columns(&mut conn, view_name, schema).await
    }

    async fn create_view(
        &self,
        _params: &ConnectionParams,
        _view_name: &str,
        _definition: &str,
        _schema: Option<&str>,
    ) -> Result<(), String> {
        Err("SQL Server: view creation disabled in Phase 1 read-only preview".into())
    }

    async fn alter_view(
        &self,
        _params: &ConnectionParams,
        _view_name: &str,
        _definition: &str,
        _schema: Option<&str>,
    ) -> Result<(), String> {
        Err("SQL Server: view alter disabled in Phase 1 read-only preview".into())
    }

    async fn drop_view(
        &self,
        _params: &ConnectionParams,
        _view_name: &str,
        _schema: Option<&str>,
    ) -> Result<(), String> {
        Err("SQL Server: view drop disabled in Phase 1 read-only preview".into())
    }

    // --- Routines -----------------------------------------------------------

    async fn get_routines(
        &self,
        params: &ConnectionParams,
        schema: Option<&str>,
    ) -> Result<Vec<RoutineInfo>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_routines(&mut conn, schema.unwrap_or("dbo")).await
    }

    async fn get_routine_parameters(
        &self,
        params: &ConnectionParams,
        routine_name: &str,
        schema: Option<&str>,
    ) -> Result<Vec<RoutineParameter>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_routine_parameters(&mut conn, routine_name, schema.unwrap_or("dbo")).await
    }

    async fn get_routine_definition(
        &self,
        params: &ConnectionParams,
        routine_name: &str,
        _routine_type: &str,
        schema: Option<&str>,
    ) -> Result<String, String> {
        let mut conn = acquire(params).await?;
        introspection::get_module_definition(&mut conn, routine_name, schema).await
    }

    // --- Query execution ---------------------------------------------------

    async fn execute_query(
        &self,
        params: &ConnectionParams,
        query: &str,
        limit: Option<u32>,
        page: u32,
        _schema: Option<&str>,
    ) -> Result<QueryResult, String> {
        use crate::drivers::common::{
            build_paginated_query_dialect, is_select_query, PaginationDialect,
        };

        let is_select = is_select_query(query);
        let mut pagination_info: Option<Pagination> = None;
        let final_query: String;

        if is_select && limit.is_some() {
            let l = limit.unwrap();
            final_query =
                build_paginated_query_dialect(query, l, page, PaginationDialect::OffsetFetch);
            pagination_info = Some(Pagination {
                page,
                page_size: l,
                total_rows: None,
                has_more: false, // filled after we know the row count
            });
        } else {
            final_query = query.to_string();
        }

        let mut conn = acquire(params).await?;
        let stream = conn
            .simple_query(final_query)
            .await
            .map_err(|e| e.to_string())?;
        let rows = stream
            .into_first_result();

        let columns: Vec<String> = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name.to_string()).collect())
            .unwrap_or_default();

        let mut json_rows: Vec<Vec<serde_json::Value>> = rows
            .iter()
            .map(|r| {
                (0..r.columns().len())
                    .map(|i| extract::extract_value(r, i))
                    .collect()
            })
            .collect();

        let mut truncated = false;
        if let Some(ref mut p) = pagination_info {
            let has_more = json_rows.len() > p.page_size as usize;
            if has_more {
                json_rows.truncate(p.page_size as usize);
            }
            p.has_more = has_more;
            truncated = has_more;
        }

        Ok(QueryResult {
            columns,
            rows: json_rows,
            affected_rows: 0,
            truncated,
            pagination: pagination_info,
        })
    }

    // --- CRUD ---------------------------------------------------------------

    async fn insert_record(
        &self,
        params: &ConnectionParams,
        table: &str,
        data: HashMap<String, serde_json::Value>,
        schema: Option<&str>,
        _max_blob_size: u64,
    ) -> Result<u64, String> {
        let mut conn = acquire(params).await?;
        let fqn = helpers::qualify(schema, table);

        // Check for identity columns
        let identity_cols = introspection::get_identity_columns(&mut conn, table, schema).await?;
        let inserting_identity = data.keys().any(|k| identity_cols.contains(k));

        let mut cols = Vec::new();
        let mut vals = Vec::new();

        for (k, v) in &data {
            cols.push(helpers::bracket_quote(k));
            vals.push(json_to_sql_literal(v));
        }

        let sql = if cols.is_empty() {
            format!("INSERT INTO {} DEFAULT VALUES", fqn)
        } else {
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                fqn,
                cols.join(", "),
                vals.join(", ")
            )
        };

        let sql = if inserting_identity {
            format!(
                "SET IDENTITY_INSERT {} ON; {}; SELECT @@ROWCOUNT AS cnt; SET IDENTITY_INSERT {} OFF",
                fqn, sql, fqn
            )
        } else {
            format!("{}; SELECT @@ROWCOUNT AS cnt", sql)
        };

        let rows = conn
            .simple_query(&sql)
            .await
            .map_err(|e| e.to_string())?
            .into_first_result();
        let count: i32 = rows.first().and_then(|r| r.get(0)).unwrap_or(0);
        Ok(count as u64)
    }

    async fn update_record(
        &self,
        params: &ConnectionParams,
        table: &str,
        pk_col: &str,
        pk_val: serde_json::Value,
        col_name: &str,
        new_val: serde_json::Value,
        schema: Option<&str>,
        _max_blob_size: u64,
    ) -> Result<u64, String> {
        let mut conn = acquire(params).await?;
        let fqn = helpers::qualify(schema, table);

        let sql = format!(
            "UPDATE {} SET {} = {} WHERE {} = {}; SELECT @@ROWCOUNT AS cnt",
            fqn,
            helpers::bracket_quote(col_name),
            json_to_sql_literal(&new_val),
            helpers::bracket_quote(pk_col),
            json_to_sql_literal(&pk_val),
        );

        let rows = conn
            .simple_query(&sql)
            .await
            .map_err(|e| e.to_string())?
            .into_first_result();
        let count: i32 = rows.first().and_then(|r| r.get(0)).unwrap_or(0);
        Ok(count as u64)
    }

    async fn delete_record(
        &self,
        params: &ConnectionParams,
        table: &str,
        pk_col: &str,
        pk_val: serde_json::Value,
        schema: Option<&str>,
    ) -> Result<u64, String> {
        let mut conn = acquire(params).await?;
        let fqn = helpers::qualify(schema, table);

        let sql = format!(
            "DELETE FROM {} WHERE {} = {}; SELECT @@ROWCOUNT AS cnt",
            fqn,
            helpers::bracket_quote(pk_col),
            json_to_sql_literal(&pk_val),
        );

        let rows = conn
            .simple_query(&sql)
            .await
            .map_err(|e| e.to_string())?
            .into_first_result();
        let count: i32 = rows.first().and_then(|r| r.get(0)).unwrap_or(0);
        Ok(count as u64)
    }

    // --- ER diagram batch ---------------------------------------------------

    async fn get_schema_snapshot(
        &self,
        params: &ConnectionParams,
        schema: Option<&str>,
    ) -> Result<Vec<TableSchema>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_schema_snapshot(&mut conn, schema.unwrap_or("dbo")).await
    }

    async fn get_all_columns_batch(
        &self,
        params: &ConnectionParams,
        schema: Option<&str>,
    ) -> Result<HashMap<String, Vec<TableColumn>>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_all_columns_batch(&mut conn, schema.unwrap_or("dbo")).await
    }

    async fn get_all_foreign_keys_batch(
        &self,
        params: &ConnectionParams,
        schema: Option<&str>,
    ) -> Result<HashMap<String, Vec<ForeignKey>>, String> {
        let mut conn = acquire(params).await?;
        introspection::get_all_foreign_keys_batch(&mut conn, schema.unwrap_or("dbo")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DatabaseSelection;

    fn make_params(host: Option<&str>, port: Option<u16>, db: &str) -> ConnectionParams {
        ConnectionParams {
            driver: "sqlserver".into(),
            host: host.map(String::from),
            port,
            username: Some("sa".into()),
            password: Some("Strong!Pass123".into()),
            database: DatabaseSelection::Single(db.into()),
            ssl_mode: None,
            ssl_ca: None,
            ssl_cert: None,
            ssl_key: None,
            ssh_enabled: None,
            ssh_connection_id: None,
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_password: None,
            ssh_key_file: None,
            ssh_key_passphrase: None,
            save_in_keychain: None,
            connection_id: None,
            trust_server_certificate: None,
            encrypt: None,
            auth_mode: None,
        }
    }

    #[test]
    fn manifest_has_phase1_capabilities() {
        let drv = SqlServerDriver::new();
        let m = drv.manifest();
        assert_eq!(m.id, "sqlserver");
        assert_eq!(m.default_port, Some(1433));
        assert!(m.is_builtin);
        assert!(!m.capabilities.readonly, "Phase 2 enables editing");
        assert!(
            !m.capabilities.manage_tables,
            "Phase 1 must hide CREATE TABLE UI"
        );
        assert!(m.capabilities.schemas);
        assert!(m.capabilities.views);
        assert!(m.capabilities.routines);
        assert_eq!(m.capabilities.auto_increment_keyword, "IDENTITY(1,1)");
        assert_eq!(m.capabilities.identifier_quote, "\"");
    }

    #[test]
    fn build_connection_url_emits_ado_net_format() {
        let drv = SqlServerDriver::new();
        let params = make_params(Some("db.internal"), Some(1445), "app");
        let url = drv.build_connection_url(&params).expect("builds");
        assert!(url.starts_with("Server=db.internal,1445;"), "got {}", url);
        assert!(url.contains("Database=app;"));
        assert!(url.contains("User Id=sa;"));
        assert!(url.contains("Password=Strong!Pass123;"));
        assert!(url.contains("TrustServerCertificate=true"));
    }

    #[test]
    fn build_connection_url_uses_defaults_when_missing() {
        let drv = SqlServerDriver::new();
        let mut params = make_params(None, None, "master");
        params.username = None;
        params.password = None;
        let url = drv.build_connection_url(&params).expect("builds");
        assert!(url.starts_with("Server=localhost,1433;"), "got {}", url);
        assert!(url.contains("Database=master;"));
        assert!(url.contains("User Id=;"));
        assert!(url.contains("Password=;"));
    }

    #[test]
    fn map_inferred_type_covers_known_kinds() {
        let drv = SqlServerDriver::new();
        assert_eq!(drv.map_inferred_type("TEXT"), "NVARCHAR(MAX)");
        assert_eq!(drv.map_inferred_type("INTEGER"), "INT");
        assert_eq!(drv.map_inferred_type("REAL"), "FLOAT");
        assert_eq!(drv.map_inferred_type("BOOLEAN"), "BIT");
        assert_eq!(drv.map_inferred_type("DATE"), "DATE");
        assert_eq!(drv.map_inferred_type("DATETIME"), "DATETIME2");
        assert_eq!(drv.map_inferred_type("JSON"), "NVARCHAR(MAX)");
    }

    #[test]
    fn map_inferred_type_passes_unknown_through() {
        let drv = SqlServerDriver::new();
        assert_eq!(drv.map_inferred_type("UUID"), "UUID");
        assert_eq!(drv.map_inferred_type("anything-custom"), "anything-custom");
    }

    #[test]
    fn get_data_types_empty_in_phase1() {
        // Phase 1 is read-only; clipboard-import types are populated in Phase 2.
        let drv = SqlServerDriver::new();
        assert!(drv.get_data_types().is_empty());
    }

    #[test]
    fn json_to_sql_literal_handles_types() {
        use serde_json::json;
        assert_eq!(json_to_sql_literal(&json!(null)), "NULL");
        assert_eq!(json_to_sql_literal(&json!(true)), "1");
        assert_eq!(json_to_sql_literal(&json!(false)), "0");
        assert_eq!(json_to_sql_literal(&json!(42)), "42");
        assert_eq!(json_to_sql_literal(&json!(3.14)), "3.14");
        assert_eq!(json_to_sql_literal(&json!("hello")), "N'hello'");
        assert_eq!(
            json_to_sql_literal(&json!("it's")),
            "N'it''s'"
        );
    }

    #[test]
    fn json_to_sql_literal_escapes_nested_quotes() {
        use serde_json::json;
        assert_eq!(
            json_to_sql_literal(&json!("O'Brien's")),
            "N'O''Brien''s'"
        );
    }

    #[test]
    fn json_to_sql_literal_handles_json_values() {
        use serde_json::json;
        let arr = json!([1, 2, 3]);
        let result = json_to_sql_literal(&arr);
        assert!(result.starts_with("N'"));
        assert!(result.ends_with("'"));

        let obj = json!({"key": "val"});
        let result = json_to_sql_literal(&obj);
        assert!(result.starts_with("N'"));
    }
}

