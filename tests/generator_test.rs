//! What the generators actually put on disk.
//!
//! Through the binary, not through a copy: this file used to re-implement
//! `to_snake_case` and `ensure_suffix` beside the tests and assert against the
//! re-implementation, so it agreed with itself whatever the crate did. The
//! transformations are private to a `[[bin]]` crate and cannot be imported —
//! `--dry-run` is how they are reached, and it is also the surface the MCP
//! wrapper consumes, so it is worth pinning for its own sake.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ream"))
}

/// An empty project — every generator refuses outside one.
struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-generator-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("fixture dir");
        std::fs::write(path.join("package.json"), r#"{"name":"fixture"}"#).expect("package.json");
        Self { path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The `{path, content}` pairs a `--dry-run` plans, in order.
fn plan(root: &Path, args: &[&str]) -> Vec<(String, String)> {
    let output = cli()
        .args(args)
        .arg("--dry-run")
        .current_dir(root)
        .output()
        .expect("run the generator");
    assert!(
        output.status.success(),
        "`ream {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 plan");
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("the plan is JSON");
    value["files"]
        .as_array()
        .expect("files is a list")
        .iter()
        .map(|file| {
            (
                file["path"].as_str().expect("a path").to_string(),
                file["content"].as_str().expect("content").to_string(),
            )
        })
        .collect()
}

/// The paths the CLI reference documents. They were wrong there for a while
/// (`app/modules/order/controllers/…`), which is the kind of claim only a test
/// reading the real output can settle.
#[test]
fn a_generated_file_lands_where_the_module_is() {
    let fixture = Fixture::new("paths");
    let planned = plan(&fixture.path, &["make:controller", "order", "Order"]);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].0, "app/order/OrderController.ts");

    let planned = plan(&fixture.path, &["make:service", "order", "Payment"]);
    assert_eq!(planned[0].0, "app/order/PaymentService.ts");

    let planned = plan(&fixture.path, &["make:entity", "order", "OrderItem"]);
    assert_eq!(planned[0].0, "app/order/OrderItem.ts");

    let planned = plan(&fixture.path, &["make:provider", "Stripe"]);
    assert_eq!(planned[0].0, "providers/StripeProvider.ts");
}

/// A suffix is added once, never twice.
#[test]
fn a_name_that_already_carries_its_suffix_keeps_one() {
    let fixture = Fixture::new("suffix");
    let planned = plan(&fixture.path, &["make:service", "order", "PaymentService"]);
    assert_eq!(planned[0].0, "app/order/PaymentService.ts");
    assert!(
        planned[0].1.contains("class PaymentService {"),
        "{}",
        planned[0].1
    );

    let planned = plan(&fixture.path, &["make:middleware", "AuthMiddleware"]);
    assert_eq!(planned[0].0, "app/middleware/auth_middleware.ts");
    assert!(
        planned[0].1.contains("class AuthMiddleware {"),
        "{}",
        planned[0].1
    );
}

/// The table an entity gets, and the file a middleware or an event is written
/// to, come out of the same snake_case rule — including the acronym case, where
/// a naive one produces `h_t_t_p_client`.
#[test]
fn a_pascal_name_becomes_a_snake_case_table_and_file() {
    let fixture = Fixture::new("snake");
    let planned = plan(&fixture.path, &["make:entity", "order", "OrderItem"]);
    assert!(
        planned[0].1.contains("@Entity('order_items')"),
        "{}",
        planned[0].1
    );

    let planned = plan(&fixture.path, &["make:middleware", "HTTPClient"]);
    assert_eq!(planned[0].0, "app/middleware/http_client_middleware.ts");

    let planned = plan(&fixture.path, &["make:event", "orderShipped"]);
    assert_eq!(planned[0].0, "app/events/order_shipped.ts");
    assert!(
        planned[0].1.contains("static eventName = 'order_shipped'"),
        "{}",
        planned[0].1
    );
}

/// `make:module` plans four files and writes none of them under `--dry-run`.
#[test]
fn the_module_umbrella_plans_four_files_and_touches_nothing() {
    let fixture = Fixture::new("module");
    let planned = plan(&fixture.path, &["make:module", "order", "Order"]);
    let paths: Vec<&str> = planned.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths.len(), 4);
    assert_eq!(paths[0], "app/order/Order.ts");
    assert_eq!(paths[1], "app/order/OrderController.ts");
    assert_eq!(paths[2], "app/order/OrderValidator.ts");
    assert!(paths[3].starts_with("database/migrations/"));
    assert!(
        !fixture.path.join("app").exists(),
        "--dry-run wrote to disk"
    );
}

/// A name that would escape the project, become a hidden file, or carry a shell
/// metacharacter is refused before anything is planned.
#[test]
fn a_name_that_could_escape_the_project_is_refused() {
    let fixture = Fixture::new("hostile");
    let hostile = format!("Order; {} /", "rm -rf");
    for args in [
        vec!["make:controller", "order", "../../etc/passwd"],
        vec!["make:controller", "../evil", "Order"],
        vec!["make:controller", "order", hostile.as_str()],
        vec!["make:provider", "_hidden"],
        vec!["make:command", ":leading"],
        vec!["make:command", "a::b"],
    ] {
        let output = cli()
            .args(&args)
            .arg("--dry-run")
            .current_dir(&fixture.path)
            .output()
            .expect("run the generator");
        assert!(
            !output.status.success(),
            "`ream {}` should have been refused",
            args.join(" ")
        );
    }
}

/// `ream new` refuses a destination it would have to clobber.
#[test]
fn new_refuses_to_clobber_an_existing_directory() {
    let fixture = Fixture::new("clobber");
    std::fs::create_dir_all(fixture.path.join("taken")).expect("dir");
    let output = cli()
        .args(["new", "taken", "--yes"])
        .current_dir(&fixture.path)
        .output()
        .expect("run new");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
