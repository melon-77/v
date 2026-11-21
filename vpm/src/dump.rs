//! Text and JSON renderings of indexes and packages, for debugging.
//!
//! The JSON uses the schema field names and value forms of
//! `apk adbdump --format json` (hex for digests and targets, dependency
//! strings as apk prints them, scripts as text), so the two can be compared
//! field by field. Absent fields, zero integers and empty lists are left
//! out; apk prints zero integers that are stored explicitly.

use std::fmt::Write as _;

use crate::adb::schema::{Acl, Dependency, Dir, FileEntry, Index, Package, PkgInfo};
use crate::digest::hex;

/// A JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Str(String),
    Num(u64),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn is_empty(&self) -> bool {
        match self {
            Json::Str(s) => s.is_empty(),
            Json::Num(n) => *n == 0,
            Json::Arr(a) => a.is_empty(),
            Json::Obj(o) => o.is_empty(),
        }
    }

    /// Pretty-printed JSON text.
    pub fn render(&self) -> String {
        let mut s = String::new();
        self.write(&mut s, 0);
        s.push('\n');
        s
    }

    fn write(&self, out: &mut String, ind: usize) {
        let pad = |out: &mut String, n: usize| {
            out.push('\n');
            for _ in 0..n {
                out.push_str("  ");
            }
        };
        match self {
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\t' => out.push_str("\\t"),
                        '\r' => out.push_str("\\r"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Num(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, ind + 1);
                    v.write(out, ind + 1);
                }
                if !a.is_empty() {
                    pad(out, ind);
                }
                out.push(']');
            }
            Json::Obj(o) => {
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, ind + 1);
                    Json::Str(k.clone()).write(out, ind + 1);
                    out.push_str(": ");
                    v.write(out, ind + 1);
                }
                if !o.is_empty() {
                    pad(out, ind);
                }
                out.push('}');
            }
        }
    }
}

struct ObjBuilder(Vec<(String, Json)>);

impl ObjBuilder {
    fn new() -> Self {
        ObjBuilder(Vec::new())
    }
    fn put(&mut self, k: &str, v: Json) -> &mut Self {
        if !v.is_empty() {
            self.0.push((k.to_string(), v));
        }
        self
    }
    fn str(&mut self, k: &str, v: &str) -> &mut Self {
        self.put(k, Json::Str(v.to_string()))
    }
    fn bytes(&mut self, k: &str, v: &[u8]) -> &mut Self {
        self.put(k, Json::Str(String::from_utf8_lossy(v).into_owned()))
    }
    fn hex(&mut self, k: &str, v: &[u8]) -> &mut Self {
        self.put(k, Json::Str(hex(v)))
    }
    fn num(&mut self, k: &str, v: u64) -> &mut Self {
        self.put(k, Json::Num(v))
    }
    fn deps(&mut self, k: &str, v: &[Dependency]) -> &mut Self {
        self.put(k, Json::Arr(v.iter().map(|d| Json::Str(d.to_string())).collect()))
    }
    fn done(&mut self) -> Json {
        Json::Obj(std::mem::take(&mut self.0))
    }
}

pub fn pkginfo_json(p: &PkgInfo) -> Json {
    ObjBuilder::new()
        .str("name", &p.name)
        .str("version", &p.version)
        .hex("hashes", &p.hashes)
        .str("description", &p.description)
        .str("arch", &p.arch)
        .str("license", &p.license)
        .str("origin", &p.origin)
        .str("maintainer", &p.maintainer)
        .str("url", &p.url)
        .hex("repo-commit", &p.repo_commit)
        .num("build-time", p.build_time)
        .num("installed-size", p.installed_size)
        .num("file-size", p.file_size)
        .num("provider-priority", p.provider_priority)
        .deps("depends", &p.depends)
        .deps("provides", &p.provides)
        .deps("replaces", &p.replaces)
        .deps("install-if", &p.install_if)
        .deps("recommends", &p.recommends)
        .num("layer", p.layer)
        .put("tags", Json::Arr(p.tags.iter().map(|t| Json::Str(t.clone())).collect()))
        .done()
}

pub fn index_json(i: &Index) -> Json {
    ObjBuilder::new()
        .str("description", &i.description)
        .put("packages", Json::Arr(i.packages.iter().map(pkginfo_json).collect()))
        .str("pkgname-spec", i.pkgname_spec.as_deref().unwrap_or(""))
        .done()
}

fn acl_json(a: &Acl) -> Json {
    let xattrs = a
        .xattrs
        .iter()
        .map(|(k, v)| Json::Str(format!("{}={}", String::from_utf8_lossy(k), hex(v))))
        .collect();
    ObjBuilder::new()
        .num("mode", a.mode as u64)
        .str("user", a.user.as_deref().unwrap_or(""))
        .str("group", a.group.as_deref().unwrap_or(""))
        .put("xattrs", Json::Arr(xattrs))
        .done()
}

fn file_json(f: &FileEntry) -> Json {
    ObjBuilder::new()
        .bytes("name", &f.name)
        .put("acl", acl_json(&f.acl))
        .num("size", f.size)
        .num("mtime", f.mtime)
        .hex("hash", &f.hash)
        .hex("target", f.target_raw.as_deref().unwrap_or(&[]))
        .done()
}

fn dir_json(d: &Dir) -> Json {
    ObjBuilder::new()
        .bytes("name", &d.name)
        .put("acl", acl_json(&d.acl))
        .put("files", Json::Arr(d.files.iter().map(file_json).collect()))
        .done()
}

pub fn package_json(p: &Package) -> Json {
    let mut scripts = ObjBuilder::new();
    for (name, body) in p.scripts.iter() {
        scripts.bytes(name, body);
    }
    ObjBuilder::new()
        .put("info", pkginfo_json(&p.info))
        .put("paths", Json::Arr(p.dirs.iter().map(dir_json).collect()))
        .put("scripts", scripts.done())
        .put("triggers", Json::Arr(p.triggers.iter().map(|t| Json::Str(t.clone())).collect()))
        .num("replaces-priority", p.replaces_priority)
        .done()
}

fn deps_line(out: &mut String, label: &str, d: &[Dependency]) {
    if !d.is_empty() {
        let v: Vec<String> = d.iter().map(|d| d.to_string()).collect();
        let _ = writeln!(out, "  {label}: {}", v.join(" "));
    }
}

/// Human-readable listing of one package's metadata.
pub fn pkginfo_text(p: &PkgInfo) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{}-{} ({})", p.name, p.version, if p.arch.is_empty() { "-" } else { &p.arch });
    if !p.description.is_empty() {
        let _ = writeln!(s, "  description: {}", p.description);
    }
    if !p.hashes.is_empty() {
        let _ = writeln!(s, "  hashes: {}", hex(&p.hashes));
    }
    for (k, v) in [("origin", &p.origin), ("license", &p.license), ("url", &p.url), ("maintainer", &p.maintainer)] {
        if !v.is_empty() {
            let _ = writeln!(s, "  {k}: {v}");
        }
    }
    let _ = writeln!(
        s,
        "  file-size: {}  installed-size: {}  build-time: {}",
        p.file_size, p.installed_size, p.build_time
    );
    if p.provider_priority != 0 {
        let _ = writeln!(s, "  provider-priority: {}", p.provider_priority);
    }
    if p.layer != 0 {
        let _ = writeln!(s, "  layer: {}", p.layer);
    }
    deps_line(&mut s, "depends", &p.depends);
    deps_line(&mut s, "provides", &p.provides);
    deps_line(&mut s, "replaces", &p.replaces);
    deps_line(&mut s, "install-if", &p.install_if);
    deps_line(&mut s, "recommends", &p.recommends);
    if !p.tags.is_empty() {
        let _ = writeln!(s, "  tags: {}", p.tags.join(" "));
    }
    s
}

/// Human-readable listing of an index.
pub fn index_text(i: &Index) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "description: {}", i.description);
    if let Some(spec) = &i.pkgname_spec {
        let _ = writeln!(s, "pkgname-spec: {spec}");
    }
    let _ = writeln!(s, "packages: {}", i.packages.len());
    for p in &i.packages {
        s.push_str(&pkginfo_text(p));
    }
    s
}

fn owner(a: &Acl) -> String {
    format!("{}:{}", a.user.as_deref().unwrap_or("?"), a.group.as_deref().unwrap_or("?"))
}

/// Human-readable listing of a package: metadata, tree, scripts.
pub fn package_text(p: &Package) -> String {
    use crate::adb::schema::FileKind;
    let mut s = pkginfo_text(&p.info);
    let _ = writeln!(
        s,
        "files: {} in {} directories, {} with data ({} bytes)",
        p.file_count(),
        p.dirs.len(),
        p.data_count(),
        p.data_bytes()
    );
    for d in &p.dirs {
        let name = if d.name.is_empty() { ".".to_string() } else { String::from_utf8_lossy(&d.name).into_owned() };
        let _ = writeln!(s, "d {:04o} {} {}", d.acl.mode, owner(&d.acl), name);
        for f in &d.files {
            let path = String::from_utf8_lossy(&d.file_path(f)).into_owned();
            let (t, extra) = match &f.kind {
                FileKind::Regular => ('f', format!("{} {}", f.size, hex(&f.hash))),
                FileKind::Hardlink(t) => ('h', format!("=> {}", String::from_utf8_lossy(t))),
                FileKind::Symlink(t) => ('l', format!("-> {}", String::from_utf8_lossy(t))),
                FileKind::CharDev(n) => ('c', format!("dev {n:#x}")),
                FileKind::BlockDev(n) => ('b', format!("dev {n:#x}")),
                FileKind::Fifo(_) => ('p', String::new()),
            };
            let _ = writeln!(s, "{t} {:04o} {} {path} {extra}", f.acl.mode, owner(&f.acl));
        }
    }
    for (name, body) in p.scripts.iter() {
        let _ = writeln!(s, "script {name}: {} bytes", body.len());
    }
    if !p.triggers.is_empty() {
        let _ = writeln!(s, "triggers: {}", p.triggers.join(" "));
    }
    if p.replaces_priority != 0 {
        let _ = writeln!(s, "replaces-priority: {}", p.replaces_priority);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_render() {
        let j = Json::Obj(vec![
            ("a".into(), Json::Str("x\"y\n\u{1}".into())),
            ("b".into(), Json::Arr(vec![Json::Num(1), Json::Num(2)])),
            ("c".into(), Json::Arr(vec![])),
        ]);
        assert_eq!(j.render(), "{\n  \"a\": \"x\\\"y\\n\\u0001\",\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": []\n}\n");
    }
}
