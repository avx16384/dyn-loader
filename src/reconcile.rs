//! # reconcile — the load-side check
//!
//! A module arrives with two things: facts about how it was produced, and the
//! bytes themselves. This module turns the facts into a verdict, and optionally
//! measures the bytes to back the verdict up.
//!
//! ## Two checks, two kinds of question
//!
//! **Policy** — *may* these two be used together. The producer's compiler and
//! version are mapped to ABI levels, and the host's are mapped too. Levels
//! either match, are bridged, or are refused. This is a statement about
//! versions, and it is the map's business, not this crate's.
//!
//! **Measurement** — *are* they the same. Both sides measure the data contract
//! and the vtable contract and compare digests. This is a statement about
//! bytes, and it does not care what the version numbers said.
//!
//! Either check alone is incomplete. Policy without measurement trusts a
//! changelog; measurement without policy has no way to say "not yet known".
//! Run both when both are available.
//!
//! ## Example
//!
//! ```ignore
//! let verdict = Verdict::check(
//!     "rustc-1.83.0-x86_64-linux",   // the module, as it recorded itself
//!     None,                          // host triple: read from the toolchain
//! )?;
//! println!("{verdict}");
//! ```

use anyhow::{Context, Result};
use dyn_abi_map::{CompilerTriple, Comparison, Contract, LevelPair, Map, Query};

/// What a host decided about a module.
#[derive(Debug, Clone)]
pub struct Verdict {
    /// The producer, as the module recorded it.
    pub module: CompilerTriple,
    /// The host's own producer facts; absent when they could not be read.
    pub host: Option<CompilerTriple>,
    /// The levels each side sits on, when both could be looked up.
    pub levels: Option<(LevelPair, LevelPair)>,
    /// The comparison, when both sides could be looked up.
    pub comparison: Option<Comparison>,
    /// The contracts that measured differently, when a probe was run.
    pub measured_breaks: Vec<Contract>,
}

impl Verdict {
    /// Check a module against this host.
    ///
    /// `host_triple` may be `None`, in which case the host's own producer
    /// facts are read from the toolchain. A host that cannot determine its
    /// own facts is not refused — the map simply cannot be consulted, and the
    /// verdict says so rather than inventing an answer.
    pub fn check(module_triple: &str, host_triple: Option<&str>) -> Result<Self> {
        let module = CompilerTriple::parse(module_triple)
            .with_context(|| format!("`{module_triple}` is not a compiler triple"))?;

        let host = match host_triple {
            Some(text) => Some(
                CompilerTriple::parse(text)
                    .with_context(|| format!("`{text}` is not a compiler triple"))?,
            ),
            None => detect_host_triple(),
        };

        let map = Map::parse(dyn_abi_map::EMBEDDED_MAP).context("the embedded ABI map is invalid")?;

        let Some(host) = host else {
            return Ok(Self {
                module,
                host: None,
                levels: None,
                comparison: None,
                measured_breaks: Vec::new(),
            });
        };

        let module_levels = map
            .lookup(&Query::from_triple(&module))
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let host_levels = map
            .lookup(&Query::from_triple(&host))
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let comparison = map.compare(&host_levels, &module_levels);

        Ok(Self {
            module,
            host: Some(host),
            levels: Some((host_levels, module_levels)),
            comparison: Some(comparison),
            measured_breaks: Vec::new(),
        })
    }

    /// Attach the result of measuring both sides.
    ///
    /// A measurement is stronger evidence than a version range: it is taken
    /// from the bytes. When it disagrees with the policy, the measurement is
    /// what a report should lead with.
    pub fn with_measurement(mut self, breaks: Vec<Contract>) -> Self {
        self.measured_breaks = breaks;
        self
    }

    /// Whether the module may be used.
    ///
    /// Refused when the policy says the levels are incompatible, or when a
    /// measurement found a break. Unknown is not the same as bad: a host that
    /// could not determine its own facts gets a permissive verdict with
    /// `checked() == false`, and it is the caller's choice what to do with
    /// that.
    pub fn loadable(&self) -> bool {
        if !self.measured_breaks.is_empty() {
            return false;
        }
        match &self.comparison {
            Some(comparison) => comparison.is_loadable(),
            None => true,
        }
    }

    /// Whether the map was actually consulted.
    pub fn checked(&self) -> bool {
        self.comparison.is_some()
    }

    /// The reason a module was refused, if it was.
    pub fn refusal(&self) -> Option<String> {
        if !self.measured_breaks.is_empty() {
            let contracts: Vec<String> = self
                .measured_breaks
                .iter()
                .map(|c| c.to_string())
                .collect();
            return Some(format!(
                "the {} contract measured differently on the two sides",
                contracts.join(" and ")
            ));
        }
        match &self.comparison {
            Some(Comparison::Incompatible(contract)) => Some(format!(
                "the {contract} contract differs and no bridge covers the pair\n  host:   {}\n  module: {}",
                self.level_text(*contract, 0),
                self.level_text(*contract, 1),
            )),
            _ => None,
        }
    }

    fn level_text(&self, contract: Contract, side: usize) -> String {
        match &self.levels {
            Some((host, module)) => {
                let levels = if side == 0 { host } else { module };
                format!(
                    "{} ({} {})",
                    levels.get(contract),
                    if side == 0 { "host" } else { "module" },
                    if side == 0 {
                        self.host.as_ref().map(|t| t.to_text()).unwrap_or_default()
                    } else {
                        self.module.to_text()
                    }
                )
            }
            None => "unknown".to_string(),
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "module produced by {}", self.module)?;
        match &self.comparison {
            Some(Comparison::Identical) => write!(f, "\n  levels match")?,
            Some(Comparison::Bridged(contract)) => {
                write!(f, "\n  {contract} contract differs but is bridged")?
            }
            Some(Comparison::Incompatible(contract)) => {
                write!(f, "\n  {contract} contract differs and is not bridged")?
            }
            None => write!(f, "\n  the map was not consulted (host facts unavailable)")?,
        }
        if !self.measured_breaks.is_empty() {
            let contracts: Vec<String> = self
                .measured_breaks
                .iter()
                .map(|c| c.to_string())
                .collect();
            write!(f, "\n  measured: {} differs", contracts.join(" and "))?;
        }
        Ok(())
    }
}

/// Read the host's own producer facts from the toolchain that built this
/// crate. Returns `None` when they cannot be determined, which is not an
/// error — it just means the map cannot be consulted.
fn detect_host_triple() -> Option<CompilerTriple> {
    if let Ok(value) = std::env::var("DYN_TRIPLE") {
        if let Some(triple) = CompilerTriple::parse(value.trim()) {
            return Some(triple);
        }
    }

    // Both facts are captured by the build script, from the toolchain that is
    // actually producing this crate. Absent rather than guessed when the
    // build environment could not supply them.
    let version = option_env!("DYN_HOST_RUSTC_VERSION")?;
    let target = option_env!("DYN_HOST_TARGET")?;
    let parts: Vec<&str> = target.split('-').collect();
    if parts.len() < 2 {
        return None;
    }
    let platform = parts[0];
    let system = parts[1..]
        .iter()
        .rev()
        .find_map(|field| match *field {
            "linux" | "gnu" | "musl" => Some("linux"),
            "windows" | "msvc" => Some("windows"),
            "darwin" | "macos" | "apple" => Some("macos"),
            "android" => Some("android"),
            "ios" => Some("ios"),
            _ => None,
        })
        .unwrap_or(parts[parts.len() - 1]);

    CompilerTriple::parse(&format!("rustc-{version}-{platform}-{system}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_matching_the_host_is_loadable() {
        let verdict = Verdict::check("rustc-1.83.0-x86_64-linux", Some("rustc-1.92.0-x86_64-linux"))
            .unwrap();
        assert!(verdict.checked());
        assert!(verdict.loadable());
        assert!(verdict.refusal().is_none());
        assert_eq!(
            verdict.comparison,
            Some(Comparison::Identical),
        );
    }

    #[test]
    fn an_unmapped_producer_is_an_error_not_a_guess() {
        let err =
            Verdict::check("cranelift-2024.1-x86_64-linux", Some("rustc-1.92.0-x86_64-linux"))
                .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("does not cover"), "{text}");
    }

    #[test]
    fn a_measured_break_refuses_even_when_the_levels_match() {
        let verdict = Verdict::check("rustc-1.83.0-x86_64-linux", Some("rustc-1.83.0-x86_64-linux"))
            .unwrap()
            .with_measurement(vec![Contract::Data]);

        assert!(verdict.checked());
        assert!(!verdict.loadable());
        let refusal = verdict.refusal().expect("a refusal reason");
        assert!(refusal.contains("data"), "{refusal}");
    }

    #[test]
    fn a_malformed_triple_is_reported_with_context() {
        let err = Verdict::check("nonsense", Some("rustc-1.92.0-x86_64-linux")).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("not a compiler triple"), "{text}");
    }

    #[test]
    fn the_verdict_reads_well_and_names_the_failing_contract() {
        let verdict =
            Verdict::check("rustc-1.83.0-x86_64-linux", Some("rustc-1.83.0-x86_64-linux")).unwrap();
        let text = verdict.to_string();
        assert!(text.contains("levels match"), "{text}");
        assert!(text.contains("rustc-1.83.0-x86_64-linux"), "{text}");
    }
}
