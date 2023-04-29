use std::collections::HashMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};

use pathdiff::diff_paths;

use crate::configuration_file::{ConfigurationFile, WriteError};
use crate::io::FromFileError;
use crate::monorepo_manifest::{EnumeratePackageManifestsError, MonorepoManifest};
use crate::opts::Action;
use crate::package_manifest::PackageManifest;
use crate::typescript_config::{
    TypescriptConfig, TypescriptParentProjectReference, TypescriptProjectReference,
};

#[derive(Debug)]
#[non_exhaustive]
pub struct LinkError {
    pub kind: LinkErrorKind,
}

impl Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            LinkErrorKind::ProjectReferencesOutOfDate => {
                write!(f, "TypeScript project references are not up-to-date")
            }
            _ => write!(f, "error linking TypeScript project references"),
        }
    }
}

impl std::error::Error for LinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            LinkErrorKind::EnumeratePackageManifests(err) => Some(err),
            LinkErrorKind::FromFile(err) => Some(err),
            LinkErrorKind::Write(err) => Some(err),
            LinkErrorKind::ProjectReferencesOutOfDate => None,
        }
    }
}

impl From<EnumeratePackageManifestsError> for LinkError {
    fn from(err: EnumeratePackageManifestsError) -> Self {
        Self {
            kind: LinkErrorKind::EnumeratePackageManifests(err),
        }
    }
}

impl From<FromFileError> for LinkError {
    fn from(err: FromFileError) -> Self {
        Self {
            kind: LinkErrorKind::FromFile(err),
        }
    }
}

impl From<WriteError> for LinkError {
    fn from(err: WriteError) -> Self {
        Self {
            kind: LinkErrorKind::Write(err),
        }
    }
}

#[derive(Debug)]
pub enum LinkErrorKind {
    #[non_exhaustive]
    EnumeratePackageManifests(EnumeratePackageManifestsError),
    #[non_exhaustive]
    FromFile(FromFileError),
    #[non_exhaustive]
    Write(WriteError),
    // FIXME: this isn't an error
    #[non_exhaustive]
    ProjectReferencesOutOfDate,
}

fn key_children_by_parent(
    mut accumulator: HashMap<PathBuf, Vec<String>>,
    package_manifest: &PackageManifest,
) -> HashMap<PathBuf, Vec<String>> {
    let mut path_so_far = PathBuf::new();
    for component in package_manifest.directory().iter() {
        let children = accumulator.entry(path_so_far.clone()).or_default();

        let new_child = component
            .to_str()
            .expect("Path not valid UTF-8 encoded")
            .to_owned();
        // DISCUSS: when would this list already contain the child?
        if !children.contains(&new_child) {
            children.push(new_child);
        }

        path_so_far.push(component);
    }
    accumulator
}

fn create_project_references(mut children: Vec<String>) -> Vec<TypescriptProjectReference> {
    // Sort the TypeScript project references for deterministic file contents.
    // This minimizes diffs since the tsconfig.json files are stored in version control.
    children.sort_unstable();
    children
        .into_iter()
        .map(|path| TypescriptProjectReference { path })
        .collect()
}

// Create a tsconfig.json file in each parent directory to an internal package.
// This permits us to compile the monorepo from the top down.
fn link_children_packages(
    root: &Path,
    action: Action,
    lerna_manifest: &MonorepoManifest,
) -> Result<bool, LinkError> {
    let mut is_exit_success = true;

    lerna_manifest
        .internal_package_manifests()?
        .iter()
        .fold(HashMap::new(), key_children_by_parent)
        .into_iter()
        .try_for_each(|(directory, children)| -> Result<(), LinkError> {
            let desired_project_references = create_project_references(children);
            let mut tsconfig = TypescriptParentProjectReference::from_directory(root, &directory)?;
            let current_project_references = &tsconfig.contents.references;
            let needs_update = !current_project_references.eq(&desired_project_references);
            if !needs_update {
                return Ok(());
            }
            if action == Action::Lint {
                is_exit_success = false;
                println!(
                    "File has out-of-date project references: {:?}, expecting:",
                    tsconfig.path()
                );
                let serialized = serde_json::to_string_pretty(&desired_project_references)
                    .expect("Should be able to serialize TypeScript project references");
                println!("{}", serialized);
                Ok(())
            } else {
                tsconfig.contents.references = desired_project_references;
                Ok(TypescriptParentProjectReference::write(root, tsconfig)?)
            }
        })?;

    Ok(is_exit_success)
}

fn link_package_dependencies(
    root: &Path,
    action: Action,
    lerna_manifest: &MonorepoManifest,
) -> Result<bool, LinkError> {
    // NOTE: this line calls LernaManifest::get_internal_package_manifests (the sloweset function) twice
    let package_manifest_by_package_name = lerna_manifest.package_manifests_by_package_name()?;

    let tsconfig_diffs: Vec<Option<TypescriptConfig>> = package_manifest_by_package_name
        .values()
        .map(|package_manifest| {
            let package_directory = package_manifest.directory();
            let mut tsconfig = TypescriptConfig::from_directory(root, &package_directory)?;
            let internal_dependencies =
                package_manifest.internal_dependencies_iter(&package_manifest_by_package_name);

            let desired_project_references: Vec<TypescriptProjectReference> = {
                let mut typescript_project_references: Vec<String> = internal_dependencies
                    .into_iter()
                    .map(|dependency| {
                        diff_paths(dependency.directory(), package_manifest.directory())
                            .expect(
                                "Unable to calculate a relative path to dependency from package",
                            )
                            .to_str()
                            .expect("Path not valid UTF-8 encoded")
                            .to_string()
                    })
                    .collect::<Vec<_>>();
                typescript_project_references.sort_unstable();

                typescript_project_references
                    .into_iter()
                    .map(|path| TypescriptProjectReference { path })
                    .collect()
            };

            // Compare the current references against the desired references
            let current_project_references = &tsconfig
                .contents
                .get("references")
                .map(|value| {
                    serde_json::from_value::<Vec<TypescriptProjectReference>>(value.clone())
                        // FIXME: this is an incorrect error message
                        .expect("Value starting as JSON should be serializable")
                })
                .unwrap_or_default();

            let needs_update = !current_project_references.eq(&desired_project_references);
            if !needs_update {
                return Ok(None);
            }

            // Update the current tsconfig with the desired references
            tsconfig.contents.insert(
                String::from("references"),
                serde_json::to_value(desired_project_references).expect(
                    "Should be able to express desired TypeScript project references as JSON",
                ),
            );

            Ok(Some(tsconfig))
        })
        .collect::<Result<Vec<Option<TypescriptConfig>>, LinkError>>()?;

    // take action on the computed diffs
    let mut is_exit_success = true;

    tsconfig_diffs
        .into_iter()
        .flatten()
        .map(|tsconfig| -> Result<(), LinkError> {
            if action == Action::Lint {
                is_exit_success = false;
                println!(
                    "File has out-of-date project references: {:?}, expecting:",
                    tsconfig.path()
                );
                let serialized = serde_json::to_string_pretty(&tsconfig.contents)
                    .expect("Should be able to serialize TypeScript project references");
                println!("{}", serialized);
                Ok(())
            } else {
                Ok(TypescriptConfig::write(root, tsconfig)?)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(is_exit_success)
}

pub fn link_typescript_project_references<P>(root: P, action: Action) -> Result<(), LinkError>
where
    P: AsRef<Path>,
{
    let root = root.as_ref();
    let lerna_manifest =
        MonorepoManifest::from_directory(root).expect("Unable to read monorepo manifest");

    let is_children_link_success = link_children_packages(root, action, &lerna_manifest)
        .expect("Unable to link children packages");

    let is_dependencies_link_success = link_package_dependencies(root, action, &lerna_manifest)
        .expect("Unable to link internal package dependencies");

    if action == Action::Lint && !(is_children_link_success && is_dependencies_link_success) {
        return Err(LinkError {
            kind: LinkErrorKind::ProjectReferencesOutOfDate,
        });
    }

    // TODO(7): create `tsconfig.settings.json` files

    Ok(())
}
