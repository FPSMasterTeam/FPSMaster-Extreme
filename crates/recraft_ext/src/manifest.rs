//! `mod.toml` manifest parsing, capability declarations, version compatibility,
//! and dependency load ordering.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// Current JS extension API version. Bumped on any breaking change to the JS
/// global API. A mod's `api` requirement is checked against this for JS mods.
pub const JS_API_VERSION: (u32, u32, u32) = (0, 1, 0);

/// Current native extension API (`recraft_ext_api`) version, checked against a
/// native mod's `api` requirement (the abi_stable layout is the second, runtime
/// safety net).
pub const NATIVE_API_VERSION: (u32, u32, u32) = (0, 1, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Js,
    Native,
}

/// Capabilities a mod declares it uses. There is no sandbox; these drive a
/// "declare + user confirm" trust model. Sensitive ones (`InjectPacket`) want
/// explicit authorization; a pure `Hud` mod is waved through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Hud,
    ReadWorld,
    ReadPlayer,
    ReadEntities,
    InjectPacket,
    Chat,
    Sound,
    Particle,
    Render,
    Input,
}

impl Capability {
    /// Whether this capability is sensitive enough to require explicit user
    /// authorization rather than being granted by default.
    pub fn is_sensitive(self) -> bool {
        matches!(self, Capability::InjectPacket)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModManifest {
    pub id: String,
    pub version: String,
    pub tier: Tier,
    /// Semver requirement against the matching API version (e.g. `^0.1`).
    pub api: String,
    /// Entry point: `main.js` for JS, the dylib filename for native.
    pub entry: String,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to parse mod.toml: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("mod '{id}' requires api {req} but host provides {have}")]
    ApiMismatch {
        id: String,
        req: String,
        have: String,
    },
    #[error("mod '{0}' depends on unknown mod '{1}'")]
    UnknownDependency(String, String),
    #[error("dependency cycle involving mod '{0}'")]
    DependencyCycle(String),
    #[error("duplicate mod id '{0}'")]
    DuplicateId(String),
}

impl ModManifest {
    pub fn parse(toml_src: &str) -> Result<Self, ManifestError> {
        Ok(toml::from_str(toml_src)?)
    }

    /// The host API version this mod's tier is checked against.
    pub fn host_api_version(&self) -> (u32, u32, u32) {
        match self.tier {
            Tier::Js => JS_API_VERSION,
            Tier::Native => NATIVE_API_VERSION,
        }
    }

    /// Whether the host satisfies this mod's `api` requirement.
    pub fn api_compatible(&self) -> bool {
        api_requirement_satisfied(&self.api, self.host_api_version())
    }

    pub fn check_api(&self) -> Result<(), ManifestError> {
        if self.api_compatible() {
            Ok(())
        } else {
            let (a, b, c) = self.host_api_version();
            Err(ManifestError::ApiMismatch {
                id: self.id.clone(),
                req: self.api.clone(),
                have: format!("{a}.{b}.{c}"),
            })
        }
    }
}

/// Minimal semver requirement check. Supports `^x.y[.z]` (caret: compatible if
/// same left-most non-zero component) and a bare `x.y[.z]` (treated as caret).
/// Good enough for the closed host/mod API contract; not a full semver engine.
pub fn api_requirement_satisfied(req: &str, have: (u32, u32, u32)) -> bool {
    let req = req.trim();
    let body = req.strip_prefix('^').unwrap_or(req);
    let want = parse_partial_version(body);
    let Some((wmaj, wmin, wpat)) = want else {
        return false;
    };
    let (hmaj, hmin, hpat) = have;
    // Caret semantics, with 0.x treated as "minor is the breaking axis".
    if wmaj != hmaj {
        return false;
    }
    if wmaj == 0 {
        // ^0.y.z := >=0.y.z, <0.(y+1).0
        if wmin != hmin {
            return false;
        }
        (hpat, hmin) >= (wpat, wmin) || hmin > wmin
    } else {
        // ^x.y.z := >=x.y.z, <(x+1).0.0
        (hmin, hpat) >= (wmin, wpat)
    }
}

fn parse_partial_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split('.');
    let maj = it.next()?.trim().parse().ok()?;
    let min = it.next().map(|v| v.trim().parse().ok()).unwrap_or(Some(0))?;
    let pat = it.next().map(|v| v.trim().parse().ok()).unwrap_or(Some(0))?;
    Some((maj, min, pat))
}

/// Topologically sort manifests by `depends` so a mod loads after everything it
/// depends on. Returns indices into `mods`. Errors on unknown deps, cycles, or
/// duplicate ids.
pub fn load_order(mods: &[ModManifest]) -> Result<Vec<usize>, ManifestError> {
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (i, m) in mods.iter().enumerate() {
        if index.insert(m.id.as_str(), i).is_some() {
            return Err(ManifestError::DuplicateId(m.id.clone()));
        }
    }
    for m in mods {
        for dep in &m.depends {
            if !index.contains_key(dep.as_str()) {
                return Err(ManifestError::UnknownDependency(m.id.clone(), dep.clone()));
            }
        }
    }

    let mut order = Vec::with_capacity(mods.len());
    let mut visited: HashSet<usize> = HashSet::new();
    let mut on_stack: HashSet<usize> = HashSet::new();

    // Iterative DFS post-order so deps precede dependents.
    for start in 0..mods.len() {
        if visited.contains(&start) {
            continue;
        }
        // (node, next-dep-index)
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        on_stack.insert(start);
        while let Some(&mut (node, ref mut di)) = stack.last_mut() {
            if *di < mods[node].depends.len() {
                let dep_id = &mods[node].depends[*di];
                *di += 1;
                let dep = index[dep_id.as_str()];
                if on_stack.contains(&dep) && !visited.contains(&dep) {
                    return Err(ManifestError::DependencyCycle(mods[node].id.clone()));
                }
                if !visited.contains(&dep) {
                    on_stack.insert(dep);
                    stack.push((dep, 0));
                }
            } else {
                visited.insert(node);
                on_stack.remove(&node);
                order.push(node);
                stack.pop();
            }
        }
    }

    Ok(order)
}
