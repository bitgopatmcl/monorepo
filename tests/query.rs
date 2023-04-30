use std::fs;

use assert_json_diff::assert_json_eq;
use testresult::TestResult;

use typescript_tools::{opts::InternalDependenciesFormat, query::query_internal_dependencies};

#[test]
fn query_snapshot_happy_path_with_format_name() -> TestResult {
    let root = "test_data/happy_path";
    let expected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("test_data/snapshots/query_name.json")?)?;
    let actual = query_internal_dependencies(root, InternalDependenciesFormat::Name)?;
    assert_json_eq!(expected, actual);
    Ok(())
}

#[test]
fn query_snapshot_happy_path_with_format_path() -> TestResult {
    let root = "test_data/happy_path";
    let expected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("test_data/snapshots/query_path.json")?)?;
    let actual = query_internal_dependencies(root, InternalDependenciesFormat::Path)?;
    assert_json_eq!(expected, actual);
    Ok(())
}
