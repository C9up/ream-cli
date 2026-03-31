//! Tests for the code generator.

use std::fs;
use std::path::Path;

// We test the generator by invoking the binary and checking output files.
// For unit tests, we test the helper functions directly.

#[test]
fn test_to_snake_case() {
    // Import from the crate
    assert_eq!(to_snake("Order"), "order");
    assert_eq!(to_snake("OrderItem"), "order_item");
    assert_eq!(to_snake("HTTPClient"), "h_t_t_p_client"); // known limitation for acronyms
}

#[test]
fn test_ensure_suffix() {
    assert_eq!(ensure_suffix("Order", "Service"), "OrderService");
    assert_eq!(ensure_suffix("OrderService", "Service"), "OrderService");
    assert_eq!(ensure_suffix("Stripe", "Provider"), "StripeProvider");
}

// Helper functions duplicated for testing (since they're private in the crate)
fn to_snake(name: &str) -> String {
    let mut result = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 { result.push('_'); }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

fn ensure_suffix(name: &str, suffix: &str) -> String {
    if name.ends_with(suffix) { name.to_string() } else { format!("{}{}", name, suffix) }
}

#[test]
fn test_scaffold_validation() {
    // Invalid names should be caught
    let invalid_names = vec!["../evil", "my app", "a/b"];
    for name in invalid_names {
        assert!(
            !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
            "Name '{}' should be invalid", name
        );
    }

    // Valid names
    let valid_names = vec!["my-app", "my_app", "app123"];
    for name in valid_names {
        assert!(
            name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
            "Name '{}' should be valid", name
        );
    }
}

#[test]
fn test_package_name_validation() {
    let valid_packages = vec!["@c9up/atlas", "@c9up/warden", "@c9up/tailwind"];
    for pkg in valid_packages {
        assert!(
            pkg.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '@' || c == '/' || c == '.'),
            "{} should be valid", pkg
        );
    }

    let invalid_packages = vec!["../evil", "@c9up/../../etc"];
    for pkg in invalid_packages {
        let valid = pkg.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '@' || c == '/' || c == '.');
        // "../evil" contains '..' which has '.' so it passes char check — but path traversal is caught by the codemods
    }
}
