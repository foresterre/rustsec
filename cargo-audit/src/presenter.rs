//! Presenter for `rustsec::Report` information.

use crate::{
    config::{DenyOption, OutputConfig, OutputFormat},
    prelude::*,
};
use abscissa_core::terminal::{
    self,
    Color::{self, Red, Yellow},
};
use rustsec::{
    cargo_lock::{
        dependency::{self, graph::EdgeDirection, Dependency},
        Lockfile, Package,
    },
    WarningKind,
};
use std::{collections::BTreeSet as Set, io, path::Path};
use std::{io::Write as _, string::ToString as _};

#[cfg(feature = "binary-scanning")]
use crate::binary_deps::BinaryReport;

/// Vulnerability information presenter
#[derive(Clone, Debug)]
pub struct Presenter {
    /// Keep track packages we've displayed once so we don't show the same dep tree
    // TODO(tarcieri): group advisories about the same package?
    displayed_packages: Set<Dependency>,

    /// Keep track of the warning kinds that correspond to deny-warnings options
    deny_warning_kinds: Set<WarningKind>,

    /// Output configuration
    config: OutputConfig,
}

impl Presenter {
    /// Create a new vulnerability information presenter
    pub fn new(config: &OutputConfig) -> Self {
        Self {
            displayed_packages: Set::new(),
            deny_warning_kinds: config
                .deny
                .iter()
                .filter_map(|k| k.get_warning_kind())
                .collect(),
            config: config.clone(),
        }
    }

    /// Information to display before a report is generated
    pub fn before_report(&mut self, path: &Path, lockfile: &Lockfile) {
        if !self.config.is_quiet() {
            status_ok!(
                "Scanning",
                "{} for vulnerabilities ({} crate dependencies)",
                path.display(),
                lockfile.packages.len(),
            );
        }
    }

    #[cfg(feature = "binary-scanning")]
    /// Information to display before a binary file is scanned
    pub fn binary_scan_report(&mut self, report: &BinaryReport, path: &Path) {
        use crate::binary_deps::BinaryReport::*;
        if !self.config.is_quiet() {
            match report {
                Complete(lockfile) => status_ok!(
                    "Found",
                    "'cargo auditable' data in {} ({} dependencies)",
                    path.display(),
                    lockfile.packages.len()
                ),
                Incomplete(lockfile) => {
                    status_warn!(
                        "{} was not built with 'cargo auditable', the report will be incomplete ({} dependencies recovered)",
                        path.display(), lockfile.packages.len());
                }
                None => status_err!(
                    "No dependency information found in {}! Is it a Rust program built with cargo?",
                    path.display(),
                ),
            }
        }
    }

    fn warning_word(&self, count: u64) -> &str {
        if count != 1 {
            "warnings"
        } else {
            "warning"
        }
    }

    /// Print the vulnerability report generated by an audit
    pub fn print_report(
        &mut self,
        report: &rustsec::Report,
        self_advisories: &[rustsec::Advisory],
        lockfile: &Lockfile,
        path: Option<&Path>
    ) {
        if self.config.format == OutputFormat::Json {
            serde_json::to_writer(io::stdout(), &report).unwrap();
            io::stdout().flush().unwrap();
            return;
        }

        // We'll set this to true if (e.g.) we see a warning and have deny-warnings enabled.
        // Once we've printed the whole report, we'll bail out of the whole program.
        let mut exit_with_failure = false;

        let tree = lockfile
            .dependency_tree()
            .expect("invalid Cargo.lock dependency tree");

        // Print out vulnerabilities and warnings
        for vulnerability in &report.vulnerabilities.list {
            self.print_vulnerability(vulnerability, &tree);
        }

        for warnings in report.warnings.values() {
            for warning in warnings.iter() {
                self.print_warning(warning, &tree)
            }
        }

        // Print out any self-advisories
        if !self_advisories.is_empty() {
            let msg = "This copy of cargo-audit has known advisories!";

            if self.config.deny.contains(&DenyOption::Warnings) {
                status_err!(msg);
            } else {
                status_warn!(msg);
            }

            for advisory in self_advisories {
                self.print_metadata(
                    &advisory.metadata,
                    self.warning_color(self.config.deny.contains(&DenyOption::Warnings)),
                );
            }
            println!();
        }

        if report.vulnerabilities.found {
            if report.vulnerabilities.count == 1 {
                match path {
                    Some(path) => status_err!("1 vulnerability found in {}", path.display()),
                    None => status_err!("1 vulnerability found!"),
                }
            } else {
                match path {
                    Some(path) => status_err!("{} vulnerabilities found in {}", report.vulnerabilities.count, path.display()),
                    None => status_err!("{} vulnerabilities found!", report.vulnerabilities.count),
                }
            }
        }

        // Count up the warnings, sorting into denied and allowed
        let mut num_denied: u64 = 0;
        let mut num_not_denied: u64 = 0;

        for (kind, warnings) in report.warnings.iter() {
            if self.deny_warning_kinds.contains(kind) {
                num_denied += warnings.len() as u64;
            } else {
                num_not_denied += warnings.len() as u64;
            }
        }

        if num_denied > 0 || num_not_denied > 0 {
            if num_denied > 0 {
                match path {
                    Some(path) => status_err!(
                        "{} denied {} found in {}",
                        num_denied,
                        self.warning_word(num_denied),
                        path.display(),
                    ),
                    None => status_err!(
                        "{} denied {} found!",
                        num_denied,
                        self.warning_word(num_denied)
                    ),
                }
                exit_with_failure = true;
            }
            if num_not_denied > 0 {
                match path {
                    Some(path) => status_warn!(
                        "{} allowed {} found in {}",
                        num_not_denied,
                        self.warning_word(num_not_denied),
                        path.display(),
                    ),
                    None => status_warn!(
                        "{} allowed {} found",
                        num_not_denied,
                        self.warning_word(num_not_denied)
                    ),
                }
            }
        }

        if !self_advisories.is_empty() {
            let upgrade_msg = "upgrade cargo-audit to the latest version: \
                               cargo install --force cargo-audit";

            if self.config.deny.contains(&DenyOption::Warnings) {
                status_err!(upgrade_msg);
                exit_with_failure = true;
            } else {
                status_warn!(upgrade_msg);
            }
        }

        // TODO(tarcieri): better unify this with vulnerabilities handling
        if exit_with_failure {
            std::process::exit(1);
        }
    }

    /// Print information about the given vulnerability
    fn print_vulnerability(
        &mut self,
        vulnerability: &rustsec::Vulnerability,
        tree: &dependency::Tree,
    ) {
        self.print_attr(Red, "Crate:    ", &vulnerability.package.name);
        self.print_attr(
            Red,
            "Version:  ",
            &vulnerability.package.version.to_string(),
        );
        self.print_metadata(&vulnerability.advisory, Red);

        if vulnerability.versions.patched().is_empty() {
            self.print_attr(Red, "Solution: ", "No fixed upgrade is available!");
        } else {
            self.print_attr(
                Red,
                "Solution: ",
                format!(
                    "Upgrade to {}",
                    vulnerability
                        .versions
                        .patched()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .as_slice()
                        .join(" OR ")
                ),
            );
        }

        self.print_tree(Red, &vulnerability.package, tree);
        println!();
    }

    /// Print information about a given warning
    fn print_warning(&mut self, warning: &rustsec::Warning, tree: &dependency::Tree) {
        let color = self.warning_color(self.deny_warning_kinds.contains(&warning.kind));

        self.print_attr(color, "Crate:    ", &warning.package.name);
        self.print_attr(color, "Version:  ", &warning.package.version.to_string());
        self.print_attr(color, "Warning:  ", warning.kind.as_str());

        if let Some(metadata) = &warning.advisory {
            self.print_metadata(metadata, color)
        }

        self.print_tree(color, &warning.package, tree);
        println!();
    }

    /// Get the color to use when displaying warnings
    fn warning_color(&self, deny_warning: bool) -> Color {
        if deny_warning {
            Red
        } else {
            Yellow
        }
    }

    /// Print a warning about a particular advisory
    fn print_metadata(&self, metadata: &rustsec::advisory::Metadata, color: Color) {
        self.print_attr(color, "Title:    ", &metadata.title);
        self.print_attr(color, "Date:     ", &metadata.date);
        self.print_attr(color, "ID:       ", &metadata.id);

        if let Some(url) = metadata.id.url() {
            self.print_attr(color, "URL:      ", &url);
        } else if let Some(url) = &metadata.url {
            self.print_attr(color, "URL:      ", url);
        }
    }

    /// Display an attribute of a particular vulnerability
    fn print_attr(&self, color: Color, attr: &str, content: impl AsRef<str>) {
        terminal::status::Status::new()
            .bold()
            .color(color)
            .status(attr)
            .print_stdout(content.as_ref())
            .unwrap();
    }

    /// Print the inverse dependency tree to standard output
    fn print_tree(&mut self, color: Color, package: &Package, tree: &dependency::Tree) {
        // Only show the tree once per package
        if !self.displayed_packages.insert(Dependency::from(package)) {
            return;
        }

        if !self.config.show_tree.unwrap_or(true) {
            return;
        }

        terminal::status::Status::new()
            .bold()
            .color(color)
            .status("Dependency tree:\n")
            .print_stdout("")
            .unwrap();

        let package_node = tree.nodes()[&Dependency::from(package)];
        tree.render(&mut io::stdout(), package_node, EdgeDirection::Incoming)
            .unwrap();
    }
}
