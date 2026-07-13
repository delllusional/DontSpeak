//! Workspace manifest invariants.
//!
//! Cargo accepts copied package metadata and direct path dependencies, so those
//! forms would build successfully while bypassing the workspace's single sources
//! of truth. Keep the policy executable as new crates are extracted.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use toml::Value;

const INHERITED_PACKAGE_KEYS: &[&str] = &[
    "version",
    "edition",
    "rust-version",
    "license",
    "repository",
    "authors",
    "publish",
];

#[test]
fn members_inherit_workspace_package_and_lint_policy() {
    let workspace = Workspace::load();
    let mut violations = Vec::new();

    assert_eq!(
        workspace.root["workspace"]["package"]["publish"].as_bool(),
        Some(false),
        "private workspace must keep workspace.package.publish = false"
    );

    for member in workspace.members.values() {
        let package = member.manifest["package"]
            .as_table()
            .unwrap_or_else(|| panic!("{} has no [package] table", member.path.display()));

        for key in INHERITED_PACKAGE_KEYS {
            let inherited = package
                .get(*key)
                .and_then(Value::as_table)
                .and_then(|value| value.get("workspace"))
                .and_then(Value::as_bool);
            if inherited != Some(true) {
                violations.push(format!(
                    "{}: package.{key}.workspace must be true",
                    member.path.display()
                ));
            }
        }

        let has_description = package
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| !description.trim().is_empty());
        if !has_description {
            violations.push(format!(
                "{}: package.description must be non-empty",
                member.path.display()
            ));
        }

        let inherits_lints = member.manifest["lints"]["workspace"].as_bool() == Some(true);
        if !inherits_lints {
            violations.push(format!(
                "{}: lints.workspace must be true",
                member.path.display()
            ));
        }
    }

    assert_no_violations(violations);
}

#[test]
fn internal_dependencies_use_workspace_inheritance() {
    let workspace = Workspace::load();
    let package_names: BTreeSet<&str> = workspace.members.keys().map(String::as_str).collect();
    let mut violations = Vec::new();

    for member in workspace.members.values() {
        check_dependency_tables(
            &member.manifest,
            &member.path,
            &package_names,
            &mut violations,
        );

        if let Some(targets) = member.manifest.get("target").and_then(Value::as_table) {
            for target in targets.values().filter_map(Value::as_table) {
                check_dependency_tables_in_table(
                    target,
                    &member.path,
                    &package_names,
                    &mut violations,
                );
            }
        }
    }

    assert_no_violations(violations);
}

#[test]
fn private_workspace_dependencies_do_not_repeat_package_versions() {
    let workspace = Workspace::load();
    let dependencies = workspace.root["workspace"]["dependencies"]
        .as_table()
        .expect("workspace must define workspace.dependencies");
    let mut violations = Vec::new();

    for (name, dependency) in dependencies {
        let Some(dependency) = dependency.as_table() else {
            continue;
        };
        if dependency.contains_key("path") && dependency.contains_key("version") {
            violations.push(format!(
                "workspace dependency {name}: private path dependency must not repeat a version"
            ));
        }
    }

    assert_no_violations(violations);
}

fn check_dependency_tables(
    manifest: &Value,
    manifest_path: &Path,
    package_names: &BTreeSet<&str>,
    violations: &mut Vec<String>,
) {
    let Some(manifest) = manifest.as_table() else {
        return;
    };
    check_dependency_tables_in_table(manifest, manifest_path, package_names, violations);
}

fn check_dependency_tables_in_table(
    table: &toml::map::Map<String, Value>,
    manifest_path: &Path,
    package_names: &BTreeSet<&str>,
    violations: &mut Vec<String>,
) {
    for table_name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(dependencies) = table.get(table_name).and_then(Value::as_table) else {
            continue;
        };
        for (dependency_name, dependency) in dependencies {
            let package_name = dependency
                .as_table()
                .and_then(|value| value.get("package"))
                .and_then(Value::as_str)
                .unwrap_or(dependency_name);
            if !package_names.contains(package_name) {
                continue;
            }

            let inherited = dependency
                .as_table()
                .and_then(|value| value.get("workspace"))
                .and_then(Value::as_bool);
            if inherited != Some(true) {
                violations.push(format!(
                    "{}: internal {table_name} entry {dependency_name} must use workspace = true",
                    manifest_path.display()
                ));
            }
        }
    }
}

fn assert_no_violations(violations: Vec<String>) {
    assert!(
        violations.is_empty(),
        "workspace manifest policy violations:\n{}",
        violations.join("\n")
    );
}

struct Member {
    path: PathBuf,
    manifest: Value,
}

struct Workspace {
    root: Value,
    members: BTreeMap<String, Member>,
}

impl Workspace {
    fn load() -> Self {
        let root_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root must resolve");
        let root_path = root_dir.join("Cargo.toml");
        let root = load_manifest(&root_path);
        let member_paths = root["workspace"]["members"]
            .as_array()
            .expect("workspace.members must be an array");
        let mut members = BTreeMap::new();

        for relative_path in member_paths {
            let relative_path = relative_path
                .as_str()
                .expect("workspace member paths must be strings");
            let path = root_dir.join(relative_path).join("Cargo.toml");
            let manifest = load_manifest(&path);
            let name = manifest["package"]["name"]
                .as_str()
                .unwrap_or_else(|| panic!("{} has no package.name", path.display()))
                .to_owned();
            assert!(
                members
                    .insert(name.clone(), Member { path, manifest })
                    .is_none(),
                "duplicate workspace package name {name}"
            );
        }

        Self { root, members }
    }
}

fn load_manifest(path: &Path) -> Value {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    toml::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}
