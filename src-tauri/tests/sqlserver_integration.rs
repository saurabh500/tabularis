//! SQL Server integration tests.
//!
//! Requires a running SQL Server instance. Set env vars:
//!   SQLSERVER_TEST_PASSWORD (required)
//!   SQLSERVER_TEST_HOST (default: 127.0.0.1)
//!   SQLSERVER_TEST_PORT (default: 1433)
//!   SQLSERVER_TEST_USER (default: sa)
//!   SQLSERVER_TEST_DB (default: master)
//!
//! Run with --test-threads=1 to avoid pool contention:
//!   cargo test --test sqlserver_integration -- --test-threads=1

use std::collections::HashMap;
use tabularis_lib::drivers::driver_trait::DatabaseDriver;
use tabularis_lib::drivers::sqlserver::SqlServerDriver;
use tabularis_lib::models::{ConnectionParams, DatabaseSelection};

fn test_params() -> ConnectionParams {
    ConnectionParams {
        driver: "sqlserver".to_string(),
        host: Some(std::env::var("SQLSERVER_TEST_HOST").unwrap_or("127.0.0.1".into())),
        port: Some(
            std::env::var("SQLSERVER_TEST_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(1433),
        ),
        username: Some(std::env::var("SQLSERVER_TEST_USER").unwrap_or("sa".into())),
        password: Some(std::env::var("SQLSERVER_TEST_PASSWORD").expect("SQLSERVER_TEST_PASSWORD required")),
        database: DatabaseSelection::Single(
            std::env::var("SQLSERVER_TEST_DB").unwrap_or("master".into()),
        ),
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
        trust_server_certificate: Some(true),
        encrypt: None,
        auth_mode: None,
    }
}

fn drv() -> SqlServerDriver {
    SqlServerDriver::new()
}

/// Helper: run a SQL statement (DDL/DML) ignoring the result set.
async fn exec_sql(d: &SqlServerDriver, params: &ConnectionParams, sql: &str) {
    d.execute_query(params, sql, None, 0, None)
        .await
        .unwrap_or_else(|e| panic!("SQL failed: {e}\nSQL: {sql}"));
}

// ─── Connection ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_connection() {
    let d = drv();
    let result = d.test_connection(&test_params()).await;
    assert!(result.is_ok(), "Connection failed: {:?}", result.err());
}

#[tokio::test]
async fn get_databases() {
    let d = drv();
    let dbs = d.get_databases(&test_params()).await.expect("get_databases failed");
    assert!(!dbs.is_empty(), "should return at least one database");
}

// ─── Schema discovery ───────────────────────────────────────────────────────

#[tokio::test]
async fn get_schemas_returns_dbo() {
    let d = drv();
    let schemas = d.get_schemas(&test_params()).await.expect("get_schemas failed");
    assert!(schemas.iter().any(|s| s == "dbo"), "'dbo' not in {:?}", schemas);
}

#[tokio::test]
async fn get_tables_and_columns() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_cols', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_cols; \
        CREATE TABLE dbo.__tab_test_cols ( \
            id INT IDENTITY(1,1) PRIMARY KEY, \
            name NVARCHAR(255) NOT NULL, \
            age INT NULL, \
            salary DECIMAL(10,2) DEFAULT 0.00 \
        )").await;

    // Tables
    let tables = d.get_tables(&p, Some("dbo")).await.expect("get_tables");
    assert!(tables.iter().any(|t| t.name == "__tab_test_cols"), "table not found");

    // Columns
    let cols = d.get_columns(&p, "__tab_test_cols", Some("dbo")).await.expect("get_columns");
    assert!(cols.len() >= 4, "expected 4+ cols, got {}", cols.len());

    let id_col = cols.iter().find(|c| c.name == "id").expect("id missing");
    assert!(id_col.is_auto_increment, "id should be identity");
    assert!(id_col.is_pk, "id should be PK");

    let name_col = cols.iter().find(|c| c.name == "name").expect("name missing");
    assert!(!name_col.is_nullable, "name should be NOT NULL");

    let age_col = cols.iter().find(|c| c.name == "age").expect("age missing");
    assert!(age_col.is_nullable, "age should be nullable");

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_cols").await;
}

// ─── Foreign keys ───────────────────────────────────────────────────────────

#[tokio::test]
async fn get_foreign_keys() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_child', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_child; \
        IF OBJECT_ID('dbo.__tab_test_parent', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_parent; \
        CREATE TABLE dbo.__tab_test_parent (id INT PRIMARY KEY); \
        CREATE TABLE dbo.__tab_test_child (id INT PRIMARY KEY, parent_id INT REFERENCES dbo.__tab_test_parent(id))").await;

    let fks = d.get_foreign_keys(&p, "__tab_test_child", Some("dbo")).await.expect("get_fks");
    assert!(!fks.is_empty(), "expected FK");
    assert_eq!(fks[0].column_name, "parent_id");
    assert_eq!(fks[0].ref_table, "__tab_test_parent");
    assert_eq!(fks[0].ref_column, "id");

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_child; DROP TABLE dbo.__tab_test_parent").await;
}

// ─── Indexes ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_indexes() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_idx', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_idx; \
        CREATE TABLE dbo.__tab_test_idx (id INT PRIMARY KEY, email NVARCHAR(200)); \
        CREATE UNIQUE INDEX IX_email ON dbo.__tab_test_idx(email)").await;

    let indexes = d.get_indexes(&p, "__tab_test_idx", Some("dbo")).await.expect("get_indexes");
    assert!(indexes.iter().any(|i| i.is_primary), "PK index missing");
    let email_idx = indexes.iter().find(|i| i.name == "IX_email").expect("IX_email missing");
    assert!(email_idx.is_unique);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_idx").await;
}

// ─── Query execution ────────────────────────────────────────────────────────

#[tokio::test]
async fn execute_query_basic() {
    let d = drv();
    let p = test_params();
    let result = d
        .execute_query(&p, "SELECT 1 AS one, 'hello' AS greeting", None, 0, None)
        .await
        .expect("query failed");

    assert_eq!(result.columns.len(), 2);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.columns[0], "one");
    assert_eq!(result.columns[1], "greeting");
}

#[tokio::test]
async fn execute_query_pagination() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_pag', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_pag; \
        CREATE TABLE dbo.__tab_test_pag (id INT PRIMARY KEY); \
        INSERT INTO dbo.__tab_test_pag VALUES (1),(2),(3),(4),(5),(6),(7),(8),(9),(10)").await;

    let result = d
        .execute_query(&p, "SELECT id FROM dbo.__tab_test_pag ORDER BY id", Some(3), 1, None)
        .await
        .expect("paginated query failed");
    assert_eq!(result.rows.len(), 3, "expected 3 rows per page");

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_pag").await;
}

// ─── Type extraction ────────────────────────────────────────────────────────

#[tokio::test]
async fn type_extraction() {
    let d = drv();
    let p = test_params();

    // Use a table with explicit column types to avoid IntN ambiguity
    // (CAST in SELECT expressions uses IntN which the bridge maps to Int4)
    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_types', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_types; \
        CREATE TABLE dbo.__tab_test_types ( \
            bit_col BIT, \
            int_col INT, \
            float_col FLOAT, \
            decimal_col DECIMAL(10,2), \
            nvarchar_col NVARCHAR(50), \
            date_col DATE, \
            time_col TIME, \
            datetime2_col DATETIME2, \
            guid_col UNIQUEIDENTIFIER, \
            null_col INT NULL \
        ); \
        INSERT INTO dbo.__tab_test_types VALUES (1, 42, 3.14, 123.45, 'hello', '2025-06-15', '14:30:00', '2025-06-15 14:30:00', NEWID(), NULL)").await;

    let result = d
        .execute_query(&p, "SELECT * FROM dbo.__tab_test_types", None, 0, Some("dbo"))
        .await
        .expect("type query failed");

    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert!(!row[0].is_null(), "bit should not be null");
    assert!(!row[1].is_null(), "int should not be null");
    assert!(!row[2].is_null(), "float should not be null");
    assert!(!row[3].is_null(), "decimal should not be null");
    assert!(!row[4].is_null(), "nvarchar should not be null");
    assert!(!row[5].is_null(), "date should not be null");
    assert!(!row[6].is_null(), "time should not be null");
    assert!(!row[7].is_null(), "datetime2 should not be null");
    assert!(!row[8].is_null(), "guid should not be null");
    assert!(row[9].is_null(), "null_col should be null");

    // Verify actual values
    assert_eq!(row[1], serde_json::json!(42));
    assert_eq!(row[4], serde_json::json!("hello"));

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_types").await;
}

// ─── INSERT ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn insert_basic() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_ins', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_ins; \
        CREATE TABLE dbo.__tab_test_ins (id INT PRIMARY KEY, name NVARCHAR(100), active BIT)").await;

    let mut data = HashMap::new();
    data.insert("id".into(), serde_json::json!(1));
    data.insert("name".into(), serde_json::json!("Alice"));
    data.insert("active".into(), serde_json::json!(true));

    let affected = d.insert_record(&p, "__tab_test_ins", data, Some("dbo"), 0).await.expect("insert");
    assert_eq!(affected, 1);

    let result = d.execute_query(&p, "SELECT * FROM dbo.__tab_test_ins WHERE id = 1", None, 0, None).await.expect("select");
    assert_eq!(result.rows.len(), 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_ins").await;
}

#[tokio::test]
async fn insert_with_identity() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_ident', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_ident; \
        CREATE TABLE dbo.__tab_test_ident (id INT IDENTITY(1,1) PRIMARY KEY, name NVARCHAR(100))").await;

    // Without identity column — auto-generate
    let mut data1 = HashMap::new();
    data1.insert("name".into(), serde_json::json!("Bob"));
    let a1 = d.insert_record(&p, "__tab_test_ident", data1, Some("dbo"), 0).await.expect("insert1");
    assert_eq!(a1, 1);

    // With identity column — IDENTITY_INSERT ON/OFF
    let mut data2 = HashMap::new();
    data2.insert("id".into(), serde_json::json!(100));
    data2.insert("name".into(), serde_json::json!("Charlie"));
    let a2 = d.insert_record(&p, "__tab_test_ident", data2, Some("dbo"), 0).await.expect("insert2");
    assert_eq!(a2, 1);

    let result = d.execute_query(&p, "SELECT * FROM dbo.__tab_test_ident ORDER BY id", None, 0, None).await.expect("select");
    assert_eq!(result.rows.len(), 2);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_ident").await;
}

#[tokio::test]
async fn insert_with_nulls() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_null', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_null; \
        CREATE TABLE dbo.__tab_test_null (id INT PRIMARY KEY, name NVARCHAR(100) NULL)").await;

    let mut data = HashMap::new();
    data.insert("id".into(), serde_json::json!(1));
    data.insert("name".into(), serde_json::Value::Null);
    let affected = d.insert_record(&p, "__tab_test_null", data, Some("dbo"), 0).await.expect("insert");
    assert_eq!(affected, 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_null").await;
}

#[tokio::test]
async fn insert_with_special_chars() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_esc', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_esc; \
        CREATE TABLE dbo.__tab_test_esc (id INT PRIMARY KEY, name NVARCHAR(200))").await;

    let mut data = HashMap::new();
    data.insert("id".into(), serde_json::json!(1));
    data.insert("name".into(), serde_json::json!("O'Brien's \"test\" value"));
    let affected = d.insert_record(&p, "__tab_test_esc", data, Some("dbo"), 0).await.expect("insert");
    assert_eq!(affected, 1);

    let result = d.execute_query(&p, "SELECT name FROM dbo.__tab_test_esc WHERE id = 1", None, 0, None).await.expect("select");
    assert_eq!(result.rows.len(), 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_esc").await;
}

#[tokio::test]
async fn insert_default_values() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_def', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_def; \
        CREATE TABLE dbo.__tab_test_def (id INT IDENTITY(1,1) PRIMARY KEY, created DATETIME2 DEFAULT GETDATE())").await;

    let data = HashMap::new(); // empty → DEFAULT VALUES
    let affected = d.insert_record(&p, "__tab_test_def", data, Some("dbo"), 0).await.expect("insert");
    assert_eq!(affected, 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_def").await;
}

// ─── UPDATE ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn update_basic() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_upd', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_upd; \
        CREATE TABLE dbo.__tab_test_upd (id INT PRIMARY KEY, name NVARCHAR(100), score INT); \
        INSERT INTO dbo.__tab_test_upd VALUES (1, 'Alice', 80), (2, 'Bob', 90)").await;

    let affected = d.update_record(&p, "__tab_test_upd", "id", serde_json::json!(1), "name", serde_json::json!("Alice Updated"), Some("dbo"), 0).await.expect("update");
    assert_eq!(affected, 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_upd").await;
}

#[tokio::test]
async fn update_to_null() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_upn', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_upn; \
        CREATE TABLE dbo.__tab_test_upn (id INT PRIMARY KEY, name NVARCHAR(100) NULL); \
        INSERT INTO dbo.__tab_test_upn VALUES (1, 'test')").await;

    let affected = d.update_record(&p, "__tab_test_upn", "id", serde_json::json!(1), "name", serde_json::Value::Null, Some("dbo"), 0).await.expect("update");
    assert_eq!(affected, 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_upn").await;
}

#[tokio::test]
async fn update_numeric_value() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_upnum', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_upnum; \
        CREATE TABLE dbo.__tab_test_upnum (id INT PRIMARY KEY, score FLOAT); \
        INSERT INTO dbo.__tab_test_upnum VALUES (1, 3.14)").await;

    let affected = d.update_record(&p, "__tab_test_upnum", "id", serde_json::json!(1), "score", serde_json::json!(99.9), Some("dbo"), 0).await.expect("update");
    assert_eq!(affected, 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_upnum").await;
}

// ─── DELETE ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_basic() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_del', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_del; \
        CREATE TABLE dbo.__tab_test_del (id INT PRIMARY KEY, name NVARCHAR(100)); \
        INSERT INTO dbo.__tab_test_del VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Charlie')").await;

    let affected = d.delete_record(&p, "__tab_test_del", "id", serde_json::json!(2), Some("dbo")).await.expect("delete");
    assert_eq!(affected, 1);

    let result = d.execute_query(&p, "SELECT COUNT(*) AS cnt FROM dbo.__tab_test_del", None, 0, None).await.expect("count");
    assert_eq!(result.rows.len(), 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_del").await;
}

#[tokio::test]
async fn delete_with_string_pk() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_delpk', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_delpk; \
        CREATE TABLE dbo.__tab_test_delpk (code NVARCHAR(10) PRIMARY KEY, val INT); \
        INSERT INTO dbo.__tab_test_delpk VALUES ('A', 1), ('B', 2)").await;

    let affected = d.delete_record(&p, "__tab_test_delpk", "code", serde_json::json!("A"), Some("dbo")).await.expect("delete");
    assert_eq!(affected, 1);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_delpk").await;
}

#[tokio::test]
async fn delete_nonexistent_row() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_delnone', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_delnone; \
        CREATE TABLE dbo.__tab_test_delnone (id INT PRIMARY KEY); \
        INSERT INTO dbo.__tab_test_delnone VALUES (1)").await;

    let affected = d.delete_record(&p, "__tab_test_delnone", "id", serde_json::json!(999), Some("dbo")).await.expect("delete");
    assert_eq!(affected, 0, "deleting nonexistent row should affect 0 rows");

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_delnone").await;
}

// ─── Views ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn views_crud() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_view', 'V') IS NOT NULL DROP VIEW dbo.__tab_test_view").await;
    exec_sql(&d, &p, "CREATE VIEW dbo.__tab_test_view AS SELECT 1 AS one").await;

    let views = d.get_views(&p, Some("dbo")).await.expect("get_views");
    assert!(views.iter().any(|v| v.name == "__tab_test_view"), "view not found");

    let defn = d.get_view_definition(&p, "__tab_test_view", Some("dbo")).await.expect("get_view_definition");
    assert!(defn.contains("SELECT"), "definition should contain SELECT: {}", defn);

    exec_sql(&d, &p, "DROP VIEW dbo.__tab_test_view").await;
}

// ─── Routines ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn routines() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_proc', 'P') IS NOT NULL DROP PROCEDURE dbo.__tab_test_proc").await;
    exec_sql(&d, &p, "CREATE PROCEDURE dbo.__tab_test_proc @name NVARCHAR(100) AS SELECT @name").await;

    let routines = d.get_routines(&p, Some("dbo")).await.expect("get_routines");
    assert!(routines.iter().any(|r| r.name == "__tab_test_proc"), "proc not found");

    let params_list = d.get_routine_parameters(&p, "__tab_test_proc", Some("dbo")).await.expect("get_params");
    assert!(!params_list.is_empty(), "should have params");

    exec_sql(&d, &p, "DROP PROCEDURE dbo.__tab_test_proc").await;
}

// ─── EXPLAIN (SHOWPLAN_XML) ─────────────────────────────────────────────────

#[tokio::test]
async fn explain_query_estimated() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_explain', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_explain; \
        CREATE TABLE dbo.__tab_test_explain (id INT PRIMARY KEY, name NVARCHAR(100))").await;

    let plan = d
        .explain_query(&p, "SELECT * FROM dbo.__tab_test_explain WHERE id = 1", false, Some("dbo"))
        .await
        .expect("explain failed");

    assert_eq!(plan.driver, "sqlserver");
    assert!(!plan.has_analyze_data);
    assert!(plan.raw_output.is_some());
    let xml = plan.raw_output.unwrap();
    assert!(xml.contains("ShowPlanXML"), "Expected SHOWPLAN_XML output: {}", &xml[..100.min(xml.len())]);

    // Should have at least one RelOp node parsed
    assert!(!plan.root.node_type.is_empty() || !plan.root.children.is_empty(),
        "Plan tree should have nodes");

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_explain").await;
}

#[tokio::test]
async fn explain_query_analyze() {
    let d = drv();
    let p = test_params();

    exec_sql(&d, &p, "\
        IF OBJECT_ID('dbo.__tab_test_analyze', 'U') IS NOT NULL DROP TABLE dbo.__tab_test_analyze; \
        CREATE TABLE dbo.__tab_test_analyze (id INT PRIMARY KEY, name NVARCHAR(100)); \
        INSERT INTO dbo.__tab_test_analyze VALUES (1, 'test')").await;

    let plan = d
        .explain_query(&p, "SELECT * FROM dbo.__tab_test_analyze WHERE id = 1", true, Some("dbo"))
        .await
        .expect("analyze failed");

    assert_eq!(plan.driver, "sqlserver");
    assert!(plan.has_analyze_data);

    exec_sql(&d, &p, "DROP TABLE dbo.__tab_test_analyze").await;
}
