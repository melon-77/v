//! Dependency resolution (stage 1: a greedy resolver that follows apk's
//! choices, not apk's solver).
//!
//! From the world and the candidates (index packages plus installed
//! ones), pick one package per name:
//! - a dependency is matched by a package's own name and version, or by a
//!   `provides` entry's name and version (§F4.1, §F4.2: the op of a
//!   provides entry is not used; an unversioned provide has version "");
//! - among the candidates for a dependency, apk's order
//!   (`compare_providers`, solver.c 537-689): when keeping, the installed
//!   one (for the same package, or when a provided version is involved);
//!   then the higher provided version of the requested name; the newer
//!   version of the same package; the higher `provider_priority` (16 bits);
//!   the installed one; the earlier repository;
//! - a provider with no provided version and no `provider_priority` is not
//!   picked for a virtual name unless its own name is required too
//!   (`is_provider_auto_selectable`);
//! - candidates that would break what is already decided are skipped: a
//!   name provided at a version by two packages (`assign_name`), a package
//!   that an active conflict (`!name` in the world or a chosen package's
//!   depends) matches, a package whose own conflicts hit the chosen set;
//! - depends are followed breadth first; a dependency already satisfied by
//!   a chosen package is not chosen again. When a later constraint cannot
//!   be met because an earlier, unconstrained choice took another version
//!   of a name, the constraint is learned and resolution starts over with
//!   it applied from the start (apk merges all constraints on a name
//!   before choosing it);
//! - `install_if` (§F5): a package whose install_if conditions are all
//!   met by the chosen set is added, trying every version of the name,
//!   best first (repeated until nothing changes);
//! - conflicts are checked once more at the end.
//!
//! The order of the result puts dependencies first; the world is taken in
//! name order (as apk sorts it) and an install_if package comes right
//! after the packages that trigger it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::adb::{Dependency, PkgInfo};
use crate::db::Installed;
use crate::error::{Error, Result};
use crate::repo::Repo;
use crate::version;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub info: PkgInfo,
    /// Repository index, `None` for an installed package no repository
    /// has.
    pub repo: Option<usize>,
    pub installed: bool,
}

pub struct Universe {
    pub cands: Vec<Candidate>,
    /// name -> (candidate, provided version)
    providers: HashMap<String, Vec<(usize, String)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keep installed versions when they satisfy the dependencies.
    Keep,
    /// Prefer the newest versions.
    Upgrade,
}

fn arch_ok(p: &PkgInfo, arches: &[String]) -> bool {
    p.arch.is_empty() || p.arch == "noarch" || arches.iter().any(|a| *a == p.arch)
}

impl Universe {
    pub fn new(repos: &[Repo], installed: &BTreeMap<String, Installed>, arch: &str) -> Universe {
        Self::with_arches(repos, installed, &[arch.to_string()])
    }

    /// With all the architectures of the root (`etc/apk/arch`, every line).
    pub fn with_arches(repos: &[Repo], installed: &BTreeMap<String, Installed>, arches: &[String]) -> Universe {
        let mut cands: Vec<Candidate> = Vec::new();
        let mut by_nv: HashMap<(String, String), usize> = HashMap::new();
        for (ri, r) in repos.iter().enumerate() {
            for p in &r.index.packages {
                if p.identity().is_none() || !arch_ok(p, arches) {
                    continue;
                }
                // first copy wins (§F4.1)
                let key = (p.name.clone(), p.version.clone());
                if by_nv.contains_key(&key) {
                    continue;
                }
                by_nv.insert(key, cands.len());
                cands.push(Candidate { info: p.clone(), repo: Some(ri), installed: false });
            }
        }
        for (name, ip) in installed {
            let key = (name.clone(), ip.meta.info.version.clone());
            match by_nv.get(&key) {
                Some(&i) => cands[i].installed = true,
                None => {
                    by_nv.insert(key, cands.len());
                    cands.push(Candidate { info: ip.meta.info.clone(), repo: None, installed: true });
                }
            }
        }
        let mut providers: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        for (i, c) in cands.iter().enumerate() {
            providers.entry(c.info.name.clone()).or_default().push((i, c.info.version.clone()));
            // a provides entry repeating the package's own name is
            // registered too (database.c 602-604)
            for p in &c.info.provides {
                providers.entry(p.name.clone()).or_default().push((i, p.version.clone()));
            }
        }
        Universe { cands, providers }
    }

    fn providers(&self, name: &str) -> &[(usize, String)] {
        self.providers.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Does a provider of `dep.name` with version `pv` satisfy `dep`
    /// (ignoring the conflict bit)?
    fn matches(dep: &Dependency, pv: &str) -> bool {
        let op = dep.op & !version::CONFLICT;
        version::version_match(pv, op, &dep.version)
    }

    /// (name, provided version) of everything candidate `c` provides: its
    /// own name and version, then its provides entries.
    fn provided(&self, c: usize) -> impl Iterator<Item = (&str, &str)> {
        let i = &self.cands[c].info;
        std::iter::once((i.name.as_str(), i.version.as_str()))
            .chain(i.provides.iter().map(|p| (p.name.as_str(), p.version.as_str())))
    }
}

/// apk keeps provider_priority in 16 bits (package.c 576).
fn prio(c: &Candidate) -> u16 {
    c.info.provider_priority as u16
}

/// The resolution.
pub struct Solution {
    /// Chosen candidates, dependencies before their dependents.
    pub order: Vec<usize>,
}

/// Why a resolution attempt stopped.
enum Fail {
    /// Start over with this constraint applied from the beginning.
    Learn(Dependency),
    Err(Error),
}

impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        Fail::Err(e)
    }
}

struct Solver<'u> {
    u: &'u Universe,
    mode: Mode,
    /// names for which the newest version is preferred even in Keep mode
    fresh: &'u HashSet<String>,
    /// constraints learned from earlier attempts: a candidate providing
    /// their name must satisfy them
    learned: &'u [Dependency],
    /// package name -> chosen candidate
    chosen: HashMap<String, usize>,
    /// chosen candidate set
    set: HashSet<usize>,
    /// who required what, for messages
    why: HashMap<usize, String>,
    /// provided name -> (chosen candidate, provided version)
    provided: HashMap<String, Vec<(usize, String)>>,
    /// conflicts in force: (dependency, the package that has it; None for
    /// the world)
    conflicts: Vec<(Dependency, Option<usize>)>,
    /// names asked for so far (a virtual provider is auto-selectable when
    /// its own name was asked for)
    requested: HashSet<String>,
    /// dependencies put back once to wait for a provider to be chosen
    deferred: HashSet<String>,
}

impl<'u> Solver<'u> {
    fn satisfied(&self, dep: &Dependency) -> Option<usize> {
        self.u
            .providers(&dep.name)
            .iter()
            .find(|(c, pv)| self.set.contains(c) && Universe::matches(dep, pv))
            .map(|(c, _)| *c)
    }

    /// Why candidate `c` cannot be chosen now, if it cannot.
    fn blocked(&self, c: usize) -> Option<String> {
        let u = self.u;
        let ci = &u.cands[c];
        if let Some(&other) = self.chosen.get(&ci.info.name) {
            if other != c {
                return Some(format!("{} is already chosen", u.cands[other].info.nv()));
            }
        }
        for (name, pv) in u.provided(c) {
            // learned constraints
            for l in self.learned.iter().filter(|l| l.name == name) {
                if !Universe::matches(l, pv) {
                    return Some(format!("it does not satisfy '{l}'"));
                }
            }
            // one versioned name, one provider (assign_name)
            for (p, ppv) in self.provided.get(name).map(|v| v.as_slice()).unwrap_or(&[]) {
                if *p != c && !(pv.is_empty() && ppv.is_empty()) {
                    return Some(format!("{} provides {name} too", u.cands[*p].info.nv()));
                }
            }
            // conflicts in force
            for (d, owner) in &self.conflicts {
                if d.name == name && *owner != Some(c) && Universe::matches(d, pv) {
                    let by = owner.map_or("the world".to_string(), |o| u.cands[o].info.nv());
                    return Some(format!("{by} conflicts with '{d}'"));
                }
            }
        }
        // its own conflicts against the chosen set
        for d in ci.info.depends.iter().filter(|d| d.is_conflict()) {
            if let Some((p, _)) =
                u.providers(&d.name).iter().find(|(p, pv)| *p != c && self.set.contains(p) && Universe::matches(d, pv))
            {
                return Some(format!("it conflicts with {} ('{d}')", u.cands[*p].info.nv()));
            }
        }
        None
    }

    /// Can `c`, providing `name` without a version, be picked for `name`?
    fn auto_selectable(&self, c: usize, name: &str, pv: &str) -> bool {
        let ci = &self.u.cands[c];
        !pv.is_empty() || prio(ci) != 0 || ci.info.name == name || self.requested.contains(&ci.info.name)
    }

    fn pick(&self, dep: &Dependency) -> Option<usize> {
        let mut best: Option<(usize, &str)> = None;
        for (c, pv) in self.u.providers(&dep.name) {
            if !Universe::matches(dep, pv) || !self.auto_selectable(*c, &dep.name, pv) || self.blocked(*c).is_some() {
                continue;
            }
            best = match best {
                Some((b, bpv)) if !self.better(&dep.name, *c, pv, b, bpv) => Some((b, bpv)),
                _ => Some((*c, pv)),
            };
        }
        best.map(|(c, _)| c)
    }

    /// Is candidate `a` (providing `name` at `apv`) better than `b`? apk's
    /// `compare_providers` order, see the module documentation.
    fn better(&self, name: &str, a: usize, apv: &str, b: usize, bpv: &str) -> bool {
        use std::cmp::Ordering::*;
        let (ca, cb) = (&self.u.cands[a], &self.u.cands[b]);
        let keep = self.mode == Mode::Keep && !self.fresh.contains(name);
        if keep
            && ca.installed != cb.installed
            && (ca.info.name == cb.info.name || !apv.is_empty() || !bpv.is_empty())
        {
            return ca.installed;
        }
        match version::cmp(apv, bpv) {
            Greater => return true,
            Less => return false,
            Equal => {}
        }
        if ca.info.name == cb.info.name {
            match version::cmp(&ca.info.version, &cb.info.version) {
                Greater => return true,
                Less => return false,
                Equal => {}
            }
        }
        if prio(ca) != prio(cb) {
            return prio(ca) > prio(cb);
        }
        if ca.installed != cb.installed {
            return ca.installed;
        }
        ca.repo.unwrap_or(usize::MAX) < cb.repo.unwrap_or(usize::MAX)
    }

    fn require(&mut self, queue: &mut VecDeque<(Dependency, String)>) -> std::result::Result<(), Fail> {
        while let Some((dep, by)) = queue.pop_front() {
            if dep.is_conflict() {
                continue; // in force since its owner was chosen
            }
            if dep.op & version::CHECKSUM == version::CHECKSUM && dep.op & version::EQUAL == 0 {
                return Err(Fail::Err(Error::Msg(format!("{by}: checksum dependency '{dep}' is not supported"))));
            }
            self.requested.insert(dep.name.clone());
            if self.satisfied(&dep).is_some() {
                continue;
            }
            if let Some(c) = self.pick(&dep) {
                self.add(c, format!("required by {by}"), queue);
                continue;
            }
            // another version of the package was taken by an earlier
            // choice: learn the constraint and start over
            if self.learnable(&dep) {
                return Err(Fail::Learn(dep));
            }
            // only virtual providers so far: wait (once) for one of them
            // to be asked for by its own name
            if !self.virtual_providers(&dep).is_empty() {
                let key = dep.to_string();
                if !self.deferred.contains(&key) {
                    self.deferred.insert(key);
                    queue.push_back((dep, by));
                    continue;
                }
                if queue.iter().any(|(d, _)| *d == dep) {
                    continue; // the deferred copy is still to come
                }
            }
            return Err(self.why_not(&dep, &by));
        }
        Ok(())
    }

    fn matching(&self, dep: &Dependency) -> Vec<(usize, &'u str)> {
        let u = self.u;
        u.providers(&dep.name).iter().filter(|(_, pv)| Universe::matches(dep, pv)).map(|(c, pv)| (*c, pv.as_str())).collect()
    }

    fn learnable(&self, dep: &Dependency) -> bool {
        !self.learned.contains(dep)
            && self.matching(dep).iter().any(|&(c, _)| {
                let n = &self.u.cands[c].info.name;
                self.chosen.get(n).map_or(false, |&o| o != c)
            })
    }

    fn virtual_providers(&self, dep: &Dependency) -> Vec<usize> {
        self.matching(dep).iter().filter(|&&(c, pv)| !self.auto_selectable(c, &dep.name, pv)).map(|&(c, _)| c).collect()
    }

    /// Explains a dependency no candidate can meet.
    fn why_not(&self, dep: &Dependency, by: &str) -> Fail {
        let u = self.u;
        let matching = self.matching(dep);
        let virt = self.virtual_providers(dep);
        let what = if u.providers(&dep.name).is_empty() {
            "no package provides it".to_string()
        } else if matching.is_empty() {
            "no available version matches".to_string()
        } else if !virt.is_empty() && virt.len() == matching.len() {
            let names: Vec<String> = virt.iter().map(|&c| u.cands[c].info.name.clone()).collect();
            format!("please select one of the 'provided by' packages explicitly: {}", names.join(", "))
        } else {
            let (c, _) = matching[0];
            format!("{}: {}", u.cands[c].info.nv(), self.blocked(c).unwrap_or_else(|| "not selectable".into()))
        };
        Fail::Err(Error::Msg(format!("unable to satisfy '{dep}' (required by {by}): {what}")))
    }

    fn add(&mut self, c: usize, why: String, queue: &mut VecDeque<(Dependency, String)>) {
        let u = self.u;
        let info = &u.cands[c].info;
        self.chosen.insert(info.name.clone(), c);
        self.set.insert(c);
        self.why.insert(c, why);
        for (name, pv) in u.provided(c) {
            self.provided.entry(name.to_string()).or_default().push((c, pv.to_string()));
        }
        let nv = info.nv();
        for d in &info.depends {
            if d.is_conflict() {
                self.conflicts.push((d.clone(), Some(c)));
            }
            queue.push_back((d.clone(), nv.clone()));
        }
    }

    /// The candidates named `name`, best first.
    fn ranked(&self, name: &str) -> Vec<usize> {
        let mut cs: Vec<usize> = Vec::new();
        for (c, _) in self.u.providers(name) {
            if self.u.cands[*c].info.name == name && !cs.contains(c) {
                cs.push(*c);
            }
        }
        // selection sort with `better` (not a total order in general, so
        // no sort_by)
        let mut out = Vec::with_capacity(cs.len());
        while !cs.is_empty() {
            let mut bi = 0;
            for i in 1..cs.len() {
                let (a, b) = (cs[i], cs[bi]);
                if self.better(name, a, &self.u.cands[a].info.version, b, &self.u.cands[b].info.version) {
                    bi = i;
                }
            }
            out.push(cs.remove(bi));
        }
        out
    }

    fn iif_met(&self, c: usize) -> bool {
        self.u.cands[c].info.install_if.iter().all(|d| {
            let hit = self.u.providers(&d.name).iter().any(|(p, pv)| self.set.contains(p) && Universe::matches(d, pv));
            hit != d.is_conflict()
        })
    }

    /// One round of install_if; true if something was added.
    fn install_if_round(&mut self, queue: &mut VecDeque<(Dependency, String)>) -> std::result::Result<bool, Fail> {
        let mut added = false;
        let mut names: Vec<&str> = self
            .u
            .cands
            .iter()
            .filter(|c| !c.info.install_if.is_empty() && !self.chosen.contains_key(&c.info.name))
            .map(|c| c.info.name.as_str())
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            if self.chosen.contains_key(name) {
                continue;
            }
            // every version of the name, best first (reconsider_name)
            for c in self.ranked(name) {
                if self.u.cands[c].info.install_if.is_empty() || self.blocked(c).is_some() || !self.iif_met(c) {
                    continue;
                }
                let info = &self.u.cands[c].info;
                let why = format!(
                    "installed because of install_if {}",
                    info.install_if.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(" ")
                );
                self.add(c, why, queue);
                self.require(queue)?;
                added = true;
                break;
            }
        }
        Ok(added)
    }

    fn check_conflicts(&self, world: &[Dependency]) -> Result<()> {
        let mut checks: Vec<(&Dependency, Option<usize>, String)> = Vec::new();
        for d in world.iter().filter(|d| d.is_conflict()) {
            checks.push((d, None, "world".into()));
        }
        for &c in &self.set {
            let info = &self.u.cands[c].info;
            for d in info.depends.iter().filter(|d| d.is_conflict()) {
                checks.push((d, Some(c), info.nv()));
            }
        }
        for (d, owner, by) in checks {
            for (p, pv) in self.u.providers(&d.name) {
                if self.set.contains(p) && Some(*p) != owner && Universe::matches(d, pv) {
                    return Err(Error::Msg(format!(
                        "conflict: {} conflicts with '{}' ({})",
                        by,
                        d,
                        self.u.cands[*p].info.nv()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Chosen set in dependency order (depth-first, dependencies first;
    /// cycles are cut where they are found). Roots: the world in name
    /// order, then the rest by name. An install_if package follows as soon
    /// as the packages it is triggered by are placed.
    fn order(&self, world: &[Dependency]) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.set.len());
        let mut state: HashMap<usize, u8> = HashMap::new(); // 1 visiting, 2 done
        let mut roots: Vec<usize> = Vec::new();
        for d in world.iter().filter(|d| !d.is_conflict()) {
            if let Some(c) = self.satisfied(d) {
                roots.push(c);
            }
        }
        let mut rest: Vec<usize> = self.set.iter().copied().collect();
        rest.sort_by(|a, b| self.u.cands[*a].info.name.cmp(&self.u.cands[*b].info.name));
        roots.extend(rest);
        let mut iif: Vec<usize> =
            self.set.iter().copied().filter(|&c| !self.u.cands[c].info.install_if.is_empty()).collect();
        iif.sort_by(|a, b| self.u.cands[*a].info.name.cmp(&self.u.cands[*b].info.name));
        for r in roots {
            self.visit(r, &iif, &mut state, &mut out);
        }
        out
    }

    fn visit(&self, c: usize, iif: &[usize], state: &mut HashMap<usize, u8>, out: &mut Vec<usize>) {
        if state.contains_key(&c) {
            return;
        }
        state.insert(c, 1);
        for d in &self.u.cands[c].info.depends {
            if d.is_conflict() {
                continue;
            }
            if let Some(p) = self.satisfied(d) {
                self.visit(p, iif, state, out);
            }
        }
        state.insert(c, 2);
        out.push(c);
        for &p in iif {
            if state.contains_key(&p) {
                continue;
            }
            let ready = self.u.cands[p].info.install_if.iter().filter(|d| !d.is_conflict()).all(|d| {
                self.u
                    .providers(&d.name)
                    .iter()
                    .any(|(q, pv)| state.get(q) == Some(&2) && Universe::matches(d, pv))
            });
            if ready {
                self.visit(p, iif, state, out);
            }
        }
    }
}

fn attempt(
    u: &Universe,
    world: &[Dependency],
    mode: Mode,
    fresh: &HashSet<String>,
    learned: &[Dependency],
) -> std::result::Result<Solution, Fail> {
    let mut s = Solver {
        u,
        mode,
        fresh,
        learned,
        chosen: HashMap::new(),
        set: HashSet::new(),
        why: HashMap::new(),
        provided: HashMap::new(),
        conflicts: world.iter().filter(|d| d.is_conflict()).map(|d| (d.clone(), None)).collect(),
        requested: HashSet::new(),
        deferred: HashSet::new(),
    };
    let mut queue: VecDeque<(Dependency, String)> = world.iter().map(|d| (d.clone(), "world".to_string())).collect();
    s.require(&mut queue)?;
    while s.install_if_round(&mut queue)? {}
    s.check_conflicts(world)?;
    Ok(Solution { order: s.order(world) })
}

/// Resolves the world. `fresh` names get their newest version even in
/// [`Mode::Keep`] (the names just added that were not installed).
pub fn solve(u: &Universe, world: &[Dependency], mode: Mode, fresh: &HashSet<String>) -> Result<Solution> {
    // apk sorts the world by name before solving
    let mut world: Vec<Dependency> = world.to_vec();
    world.sort_by(|a, b| a.name.cmp(&b.name));
    let mut learned: Vec<Dependency> = Vec::new();
    loop {
        match attempt(u, &world, mode, fresh, &learned) {
            Ok(s) => return Ok(s),
            Err(Fail::Learn(d)) if learned.len() < 64 => learned.push(d),
            Err(Fail::Learn(d)) => {
                return Err(Error::Msg(format!("unable to satisfy '{d}': too many conflicting constraints")))
            }
            Err(Fail::Err(e)) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::Index;
    use crate::verify::TrustInfo;

    fn pkg(name: &str, ver: &str, deps: &[&str], provides: &[&str], iif: &[&str]) -> PkgInfo {
        PkgInfo {
            name: name.into(),
            version: ver.into(),
            hashes: vec![1; 32],
            arch: "x86_64".into(),
            depends: deps.iter().map(|d| Dependency::parse(d).unwrap()).collect(),
            provides: provides.iter().map(|d| Dependency::parse(d).unwrap()).collect(),
            install_if: iif.iter().map(|d| Dependency::parse(d).unwrap()).collect(),
            ..Default::default()
        }
    }

    fn universe(pkgs: Vec<PkgInfo>) -> Universe {
        universe_with(pkgs, &[])
    }

    fn universe_with(pkgs: Vec<PkgInfo>, installed: &[PkgInfo]) -> Universe {
        let repo = Repo {
            name: "t".into(),
            index_url: "t/Packages.adb".into(),
            base: "t".into(),
            default_spec: "${name}-${version}.apk".into(),
            remote: false,
            index: Index { packages: pkgs, ..Default::default() },
            trust: TrustInfo::default(),
            generation: 0,
            stale: false,
        };
        let inst: BTreeMap<String, Installed> = installed
            .iter()
            .map(|i| {
                let meta = crate::db::PkgMeta { info: i.clone(), ..Default::default() };
                (i.name.clone(), Installed { meta, dir: std::path::PathBuf::new() })
            })
            .collect();
        Universe::new(&[repo], &inst, "x86_64")
    }

    fn names(u: &Universe, s: &Solution) -> Vec<String> {
        s.order.iter().map(|&c| u.cands[c].info.nv()).collect()
    }

    fn w(v: &[&str]) -> Vec<Dependency> {
        v.iter().map(|d| Dependency::parse(d).unwrap()).collect()
    }

    fn set(u: &Universe, world: &[&str]) -> std::result::Result<Vec<String>, String> {
        let none = HashSet::new();
        let mut v = solve(u, &w(world), Mode::Keep, &none).map(|s| names(u, &s)).map_err(|e| e.to_string())?;
        v.sort();
        Ok(v)
    }

    #[test]
    fn upgrade_through_provides() {
        // zlib is only reached through so:libz.so.1, provided at the same
        // version by both builds: upgrade takes the newer build, keep the
        // installed one
        let old = pkg("zlib", "1.3-r1", &[], &["so:libz.so.1=1.3"], &[]);
        let new = pkg("zlib", "1.3-r2", &[], &["so:libz.so.1=1.3"], &[]);
        let app = pkg("app", "1.0", &["so:libz.so.1"], &[], &[]);
        let u = universe_with(vec![app, old.clone(), new], &[old]);
        let none = HashSet::new();
        assert!(names(&u, &solve(&u, &w(&["app"]), Mode::Upgrade, &none).unwrap()).contains(&"zlib-1.3-r2".to_string()));
        assert!(names(&u, &solve(&u, &w(&["app"]), Mode::Keep, &none).unwrap()).contains(&"zlib-1.3-r1".to_string()));
    }

    #[test]
    fn basic() {
        let u = universe(vec![
            pkg("app", "1.0", &["so:libfoo.so.1", "cmd:sh"], &[], &[]),
            pkg("libfoo", "1.0", &[], &["so:libfoo.so.1=0"], &[]),
            pkg("libfoo", "1.1", &[], &["so:libfoo.so.1=0"], &[]),
            pkg("busybox", "1.0", &[], &["cmd:sh=1.0"], &[]),
            pkg("app-doc", "1.0", &[], &[], &["app", "doc"]),
            pkg("app-bash", "1.0", &["bash"], &[], &["app", "bash"]),
            pkg("doc", "1.0", &[], &[], &[]),
        ]);
        let none = HashSet::new();
        let s = solve(&u, &w(&["app"]), Mode::Upgrade, &none).unwrap();
        assert_eq!(names(&u, &s), vec!["libfoo-1.1", "busybox-1.0", "app-1.0"]);
        let s = solve(&u, &w(&["app", "doc"]), Mode::Upgrade, &none).unwrap();
        assert!(names(&u, &s).contains(&"app-doc-1.0".to_string()));
        assert!(solve(&u, &w(&["app", "libfoo<1.1", "libfoo>1.0"]), Mode::Upgrade, &none).is_err());
        let s = solve(&u, &w(&["libfoo<1.1", "app"]), Mode::Upgrade, &none).unwrap();
        assert!(names(&u, &s).contains(&"libfoo-1.0".to_string()));
        assert!(solve(&u, &w(&["app", "!busybox"]), Mode::Upgrade, &none).is_err());
        assert!(solve(&u, &w(&["nothere"]), Mode::Upgrade, &none).is_err());
    }

    /// The synthetic cases where stage 1's first resolver differed from
    /// apk (spec review, /root/catalyst-test/cr): apk's answers.
    #[test]
    fn like_apk() {
        let u = universe(vec![
            pkg("t1-app", "1-r0", &["t1-virt"], &[], &[]),
            PkgInfo { provider_priority: 1, ..pkg("t1-a", "1-r0", &[], &["t1-virt=2"], &[]) },
            PkgInfo { provider_priority: 100, ..pkg("t1-b", "1-r0", &[], &["t1-virt=1"], &[]) },
            pkg("t2-app", "1-r0", &["t2-virt"], &[], &[]),
            pkg("t2-a", "1-r0", &[], &["t2-virt"], &[]),
            pkg("t3-a", "1-r0", &[], &["so:libt3.so.1=1"], &[]),
            pkg("t3-b", "1-r0", &[], &["so:libt3.so.1=1"], &[]),
            pkg("t4-app", "1-r0", &["t4-x"], &[], &[]),
            pkg("t4-x", "1-r0", &[], &[], &[]),
            pkg("t4-y", "1-r0", &[], &["t4-x=2"], &[]),
            pkg("t5-app", "1-r0", &["t5-virt", "!t5-a"], &[], &[]),
            pkg("t5-a", "1-r0", &[], &["t5-virt=2"], &[]),
            pkg("t5-b", "1-r0", &[], &["t5-virt=1"], &[]),
            pkg("t6-app", "1-r0", &["t6-lib"], &[], &[]),
            pkg("t6-lib", "1-r0", &[], &[], &[]),
            pkg("t6-lib", "2-r0", &[], &[], &[]),
            pkg("t6-other", "1-r0", &["t6-lib<2"], &[], &[]),
            pkg("t7-foo", "1-r0", &[], &[], &[]),
            pkg("t7-foo", "2-r0", &[], &[], &[]),
            pkg("t7-docs", "1-r0", &[], &[], &[]),
            pkg("t7-foo-doc", "1-r0", &[], &[], &["t7-foo=1-r0", "t7-docs"]),
            pkg("t7-foo-doc", "2-r0", &[], &[], &["t7-foo=2-r0", "t7-docs"]),
            pkg("t8-a", "1-r0", &[], &["t8-virt=3"], &[]),
            pkg("t8-ext", "1-r0", &[], &[], &["t8-virt>=2"]),
            pkg("t10-app", "1-r0", &["t10-lib>=1"], &[], &[]),
            pkg("t10-lib", "1-r0", &[], &[], &[]),
            pkg("t10-lib", "2-r0", &[], &[], &[]),
            pkg("t10-lib", "3-r0", &[], &[], &[]),
        ]);
        assert_eq!(set(&u, &["t1-app"]).unwrap(), ["t1-a-1-r0", "t1-app-1-r0"]);
        assert!(set(&u, &["t2-app"]).unwrap_err().contains("explicitly"));
        assert_eq!(set(&u, &["t2-app", "t2-a"]).unwrap(), ["t2-a-1-r0", "t2-app-1-r0"]);
        assert!(set(&u, &["t3-a", "t3-b"]).is_err());
        assert_eq!(set(&u, &["t4-app"]).unwrap(), ["t4-app-1-r0", "t4-y-1-r0"]);
        assert_eq!(set(&u, &["t5-app"]).unwrap(), ["t5-app-1-r0", "t5-b-1-r0"]);
        assert_eq!(set(&u, &["t6-app", "t6-lib<2"]).unwrap(), ["t6-app-1-r0", "t6-lib-1-r0"]);
        assert_eq!(set(&u, &["t6-app", "t6-other"]).unwrap(), ["t6-app-1-r0", "t6-lib-1-r0", "t6-other-1-r0"]);
        assert_eq!(set(&u, &["t7-foo<2", "t7-docs"]).unwrap(), ["t7-docs-1-r0", "t7-foo-1-r0", "t7-foo-doc-1-r0"]);
        assert_eq!(set(&u, &["t8-a"]).unwrap(), ["t8-a-1-r0", "t8-ext-1-r0"]);
        assert_eq!(set(&u, &["t10-app", "t10-lib<3"]).unwrap(), ["t10-app-1-r0", "t10-lib-2-r0"]);
        assert_eq!(set(&u, &["t10-lib<3", "t10-app"]).unwrap(), ["t10-app-1-r0", "t10-lib-2-r0"]);
    }

    #[test]
    fn keep_installed_on_add() {
        // R7: an installed name stays at its version unless it is "fresh"
        let x1 = pkg("x", "1-r0", &[], &[], &[]);
        let u = universe_with(vec![x1.clone(), pkg("x", "2-r0", &[], &[], &[])], &[x1]);
        let none = HashSet::new();
        assert_eq!(names(&u, &solve(&u, &w(&["x"]), Mode::Keep, &none).unwrap()), ["x-1-r0"]);
        assert_eq!(names(&u, &solve(&u, &w(&["x"]), Mode::Upgrade, &none).unwrap()), ["x-2-r0"]);
    }
}
