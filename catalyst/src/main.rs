//! catalyst command line: the install engine (`add`, `del`, `upgrade`, `list`)
//! and the file-level commands (`verify`, `dump-index`, `dump`,
//! `manifest`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use catalyst::adb::{self, EntryKind, FileKind, SCHEMA_INDEX, SCHEMA_PACKAGE};
use catalyst::digest::{self, Alg, Hasher};
use catalyst::dump;
use catalyst::engine;
use catalyst::verify::{self, TrustedKeys, Verified, VerifyOptions};
use catalyst::Error;

const USAGE: &str = "\
usage: oct [OPTIONS] COMMAND [ARGS]

options:
  --root DIR             root to manage (default /)
  --repository INDEX     use ONLY these Packages.adb paths/URLs (repeatable;
                         replaces ROOT/etc/apk/repositories)
  --add-repository INDEX, -X INDEX
                         add a Packages.adb path or URL in addition to
                         ROOT/etc/apk/repositories (repeatable)
  --keys-dir DIR         trusted public keys (default ROOT/etc/apk/keys,
                         then ROOT/lib/apk/keys)
  --arch ARCH            override target architecture
  --print-arch           print the architecture of ROOT and exit
  --initdb               create catalyst's database in ROOT
  --plain                plain progress lines instead of the full-screen view
  --simulate, -s         show what add/del/upgrade/update/fix/cache clean
                         would do without changing anything
  --upgrade, -u          with add: upgrade named packages if already installed
  --sync                 do not return before the change is on disk (by
                         default the change is prepared on disk, then made;
                         a power cut right after it leaves it undone or to
                         be finished by the next run, never half done)
  --no-scripts           do not run package scripts, triggers or commit hooks
  --no-commit-hooks      do not run commit hooks (pre-commit / post-commit)
  --jobs N               fetch/unpack threads (default: one per CPU, max 16)
  --allow-rollback       accept a repository index older than the one used
                         before (refused by default: replay protection)
  --allow-untrusted      accept unsigned or untrusted files (testing only)

commands:
  add [-u] NAME...       add to the world and install (NAME may carry a
                         version constraint, e.g. 'foo>=1.2')
  del NAME...            remove from the world; uninstall what is no longer
                         needed
  upgrade                upgrade everything to the newest versions
  update                 refresh repository indexes
  list                   list the installed packages ([world] = asked for)
  search [-x] [-d] [PAT...]
                         search repositories by name or provides
                         (-x exact match, -d include descriptions)
  info [-e|-a|-d|-L|-R|-P|-s|-W] [NAME...]
                         show package information (-e installed check,
                         -W who owns PATH; no args lists installed names)
  fix [NAME...]          verify and repair live-tree files from /Programs
  recover                finish a change that was interrupted (every command
                         does this first; on its own it is for system start)
  cache clean            remove downloaded .apk files from the cache
  verify FILE...         check signature and contents; print a summary
  dump-index FILE [--json]
                         print an index (JSON as `apk adbdump --format json`)
  dump FILE [--json]     print an index or a package's metadata
  manifest FILE          list a package's entries with the SHA-256 of the
                         content read from its DATA blocks

layout of a root managed by catalyst:
  ROOT/Programs/NAME/VERSION/   the package's files (+ .catalyst/ metadata)
  ROOT/Programs/NAME/Current    symlink to the version in use
  the usual tree: hard links to Programs (copies under etc/ and boot/)
  ROOT/var/lib/catalyst/             world, journal, lock
  ROOT/var/cache/catalyst/           downloaded packages
";

struct Opts {
    root: PathBuf,
    repositories: Vec<String>,
    extra_repositories: Vec<String>,
    keys_dir: Option<PathBuf>,
    arch: Option<String>,
    print_arch: bool,
    initdb: bool,
    plain: bool,
    allow_untrusted: bool,
    allow_rollback: bool,
    no_scripts: bool,
    no_commit_hooks: bool,
    simulate: bool,
    upgrade: bool,
    sync: bool,
    jobs: usize,
    json: bool,
    search: engine::SearchOpts,
    info: engine::InfoOpts,
    cmd: String,
    args: Vec<String>,
}

impl Opts {
    fn engine(&self) -> engine::Options {
        engine::Options {
            root: self.root.clone(),
            repositories: self.repositories.clone(),
            extra_repositories: self.extra_repositories.clone(),
            keys_dir: self.keys_dir.clone(),
            arch: self.arch.clone(),
            initdb: self.initdb,
            plain: self.plain,
            allow_untrusted: self.allow_untrusted,
            allow_rollback: self.allow_rollback,
            no_scripts: self.no_scripts,
            no_commit_hooks: self.no_commit_hooks,
            simulate: self.simulate,
            upgrade: self.upgrade,
            jobs: self.jobs,
            crash_at: std::env::var("CATALYST_CRASH_AT").ok().filter(|s| !s.is_empty()),
            sync: self.sync,
        }
    }
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        root: PathBuf::from("/"),
        repositories: Vec::new(),
        extra_repositories: Vec::new(),
        keys_dir: None,
        arch: None,
        print_arch: false,
        initdb: false,
        plain: false,
        allow_untrusted: false,
        allow_rollback: false,
        no_scripts: false,
        no_commit_hooks: false,
        simulate: false,
        upgrade: false,
        sync: false,
        jobs: 0,
        json: false,
        search: engine::SearchOpts::default(),
        info: engine::InfoOpts::default(),
        cmd: String::new(),
        args: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    let mut positional = Vec::new();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let cmd = positional.first().map(|s: &String| s.as_str()).unwrap_or("");
        match a.as_str() {
            "--root" | "-p" => o.root = PathBuf::from(val("--root")?),
            "--repository" => o.repositories.push(val("--repository")?),
            "--add-repository" | "-X" => o.extra_repositories.push(val("--add-repository")?),
            "--keys-dir" => o.keys_dir = Some(PathBuf::from(val("--keys-dir")?)),
            "--arch" => o.arch = Some(val("--arch")?),
            "--print-arch" => o.print_arch = true,
            "--initdb" => o.initdb = true,
            "--plain" => o.plain = true,
            "--no-scripts" => o.no_scripts = true,
            "--no-commit-hooks" => o.no_commit_hooks = true,
            "--simulate" => o.simulate = true,
            "--upgrade" | "-u" => o.upgrade = true,
            "--sync" => o.sync = true,
            "--jobs" | "-j" => {
                o.jobs = val("--jobs")?.parse().map_err(|_| "--jobs needs a number".to_string())?
            }
            "--allow-untrusted" => o.allow_untrusted = true,
            "--allow-rollback" => o.allow_rollback = true,
            "--json" => o.json = true,
            "-s" if cmd == "info" => o.info.size = true,
            "-s" => o.simulate = true,
            "-x" | "--exact" if cmd == "search" => o.search.exact = true,
            "-d" | "--description" if cmd == "search" => o.search.description = true,
            "-e" | "--installed" if cmd == "info" => o.info.installed = true,
            "-a" | "--all" if cmd == "info" => o.info.all = true,
            "-d" | "--description" if cmd == "info" => o.info.description = true,
            "-L" | "--contents" if cmd == "info" => o.info.contents = true,
            "-R" | "--depends" if cmd == "info" => o.info.depends = true,
            "-P" | "--provides" if cmd == "info" => o.info.provides = true,
            "--size" if cmd == "info" => o.info.size = true,
            "-W" | "--who-owns" if cmd == "info" => o.info.who_owns = true,
            "-h" | "--help" => return Err(String::new()),
            "--" => positional.extend(it.by_ref()),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => positional.push(a),
        }
    }
    if o.print_arch {
        return Ok(o);
    }
    if positional.is_empty() {
        return Err(String::new());
    }
    o.cmd = positional.remove(0);
    o.args = positional;
    Ok(o)
}

fn load_keys(o: &Opts) -> Result<TrustedKeys, Error> {
    let keys = match &o.keys_dir {
        Some(d) => TrustedKeys::from_dir(d)?,
        None => TrustedKeys::from_root(&o.root)?,
    };
    for (name, why) in &keys.skipped {
        eprintln!("oct: warning: key file {name} skipped: {why}");
    }
    if keys.is_empty() && !o.allow_untrusted {
        eprintln!("oct: warning: no trusted keys loaded");
    }
    Ok(keys)
}

fn vopts(o: &Opts) -> VerifyOptions {
    VerifyOptions { allow_untrusted: o.allow_untrusted, ..Default::default() }
}

/// name-version -> identity, from the indexes given with --repository
/// (local paths only in this stage).
fn load_identities(o: &Opts, keys: &TrustedKeys) -> Result<HashMap<String, Vec<u8>>, Error> {
    let mut m = HashMap::new();
    for r in &o.repositories {
        let p = r.strip_prefix("file://").unwrap_or(r);
        if p.contains("://") {
            return Err(Error::Format(format!("{r}: remote repositories are not supported by this command yet")));
        }
        let (idx, _) = adb::read_index_file(Path::new(p), keys, &vopts(o))?;
        for pkg in idx.packages {
            m.entry(pkg.nv()).or_insert(pkg.hashes);
        }
    }
    Ok(m)
}

fn cmd_verify(o: &Opts) -> Result<bool, Error> {
    if o.args.is_empty() {
        return Err(Error::Format("verify needs at least one FILE".into()));
    }
    let keys = load_keys(o)?;
    let ids = load_identities(o, &keys)?;
    let mut all_ok = true;
    for f in &o.args {
        let line = match verify::verify_file(Path::new(f), &keys, &vopts(o)) {
            Ok(Verified::Index { description, packages, compression, trust }) => format!(
                "{f}: OK: index (schema indx) '{description}', {packages} packages, compression {compression}, {}\n",
                trust.describe()
            ),
            Ok(Verified::Package { name, version, arch, dirs, files, data_blocks, data_bytes, identity, compression, trust }) => {
                let nv = format!("{name}-{version}");
                let (status, idx) = match ids.get(&nv) {
                    Some(h) if h.len() == 32 && h[..] == identity[..] => ("OK", ", matches the index".to_string()),
                    Some(h) if h.len() == 32 => {
                        all_ok = false;
                        ("FAILED", format!(", DOES NOT MATCH the index ({})", digest::hex(h)))
                    }
                    Some(_) => ("OK", ", index hash not comparable".to_string()),
                    None if !ids.is_empty() => ("OK", ", not in the index".to_string()),
                    None => ("OK", String::new()),
                };
                format!(
                    "{f}: {status}: package (schema pckg) {nv} ({arch}), {files} files in {dirs} directories, {data_blocks} data blocks ({data_bytes} bytes) verified, identity sha256:{}{idx}, compression {compression}, {}\n",
                    digest::hex(&identity),
                    trust.describe()
                )
            }
            Err(e) => {
                all_ok = false;
                format!("{f}: FAILED: {e}\n")
            }
        };
        out(&line)?;
    }
    Ok(all_ok)
}

fn out(s: &str) -> Result<(), Error> {
    let mut so = std::io::stdout().lock();
    so.write_all(s.as_bytes())?;
    so.flush()?;
    Ok(())
}

fn cmd_dump(o: &Opts, index_only: bool) -> Result<bool, Error> {
    let [f] = &o.args[..] else {
        return Err(Error::Format(format!("{} needs one FILE", o.cmd)));
    };
    let keys = load_keys(o)?;
    let path = Path::new(f);
    let schema = adb::file_schema(path)?;
    match schema {
        SCHEMA_INDEX => {
            let (idx, trust) = adb::read_index_file(path, &keys, &vopts(o))?;
            if o.json {
                out(&dump::index_json(&idx).render())?;
            } else {
                out(&format!("# {}\n", trust.describe()))?;
                out(&dump::index_text(&idx))?;
            }
        }
        SCHEMA_PACKAGE if !index_only => {
            let pr = adb::open_package_file(path, &keys, &vopts(o))?;
            if o.json {
                out(&dump::package_json(pr.package()).render())?;
            } else {
                out(&format!("# {} (contents not read; use verify)\n", pr.trust().describe()))?;
                out(&dump::package_text(pr.package()))?;
            }
        }
        s => return Err(Error::Schema(format!("{f}: unexpected schema '{}'", adb::schema_name(s)))),
    }
    Ok(true)
}

fn cmd_manifest(o: &Opts) -> Result<bool, Error> {
    let [f] = &o.args[..] else {
        return Err(Error::Format("manifest needs one FILE".into()));
    };
    let keys = load_keys(o)?;
    let mut pr = adb::open_package_file(Path::new(f), &keys, &vopts(o))?;
    let mut so = std::io::BufWriter::new(std::io::stdout().lock());
    let mut content: HashMap<Vec<u8>, String> = HashMap::new();
    let mut buf = vec![0u8; 128 * 1024];
    while let Some(mut e) = pr.next_entry()? {
        let path = e.path.to_vec();
        let ps = String::from_utf8_lossy(&path).into_owned();
        if e.kind == EntryKind::Dir {
            let a = &e.dir.acl;
            writeln!(so, "d\t{:04o}\t{}\t{}\t0\t0\t{ps}\t", a.mode, a.user.as_deref().unwrap_or(""), a.group.as_deref().unwrap_or(""))?;
            continue;
        }
        let fe = e.file.unwrap().clone();
        let a = &fe.acl;
        let (t, extra) = match &fe.kind {
            FileKind::Regular => {
                let mut h = Hasher::new(Alg::Sha256);
                if let Some(d) = e.data() {
                    loop {
                        let n = d.read(&mut buf).map_err(|err| Error::Integrity(format!("{ps}: {err}")))?;
                        if n == 0 {
                            break;
                        }
                        h.update(&buf[..n]);
                    }
                }
                let x = digest::hex(&h.finish());
                content.insert(path.clone(), x.clone());
                ('f', x)
            }
            FileKind::Hardlink(t) => ('h', content.get(t).cloned().unwrap_or_default()),
            FileKind::Symlink(t) => ('l', String::from_utf8_lossy(t).into_owned()),
            FileKind::CharDev(n) => ('c', format!("{n}")),
            FileKind::BlockDev(n) => ('b', format!("{n}")),
            FileKind::Fifo(n) => ('p', format!("{n}")),
        };
        let mtime = if matches!(fe.kind, FileKind::Regular | FileKind::Hardlink(_)) { fe.mtime } else { 0 };
        writeln!(
            so,
            "{t}\t{:04o}\t{}\t{}\t{}\t{mtime}\t{ps}\t{extra}",
            a.mode,
            a.user.as_deref().unwrap_or(""),
            a.group.as_deref().unwrap_or(""),
            if t == 'l' { 0 } else { fe.size }
        )?;
    }
    pr.finish()?;
    so.flush()?;
    Ok(true)
}

fn main() -> ExitCode {
    catalyst::sys::note_umask();
    catalyst::prof::mark("start");
    let o = match parse_args() {
        Ok(o) => o,
        Err(m) => {
            if !m.is_empty() {
                eprintln!("oct: {m}");
            }
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if o.print_arch {
        let arch = o.arch.clone().unwrap_or_else(|| catalyst::repo::root_arch(&o.root));
        println!("{arch}");
        return ExitCode::SUCCESS;
    }
    let r = match o.cmd.as_str() {
        "verify" => cmd_verify(&o),
        "dump-index" => cmd_dump(&o, true),
        "dump" => cmd_dump(&o, false),
        "manifest" => cmd_manifest(&o),
        "add" => engine::add(&o.engine(), &o.args),
        "del" => engine::del(&o.engine(), &o.args),
        "upgrade" | "list" | "update" | "recover" if !o.args.is_empty() => {
            eprintln!("oct: '{}' takes no arguments", o.cmd);
            return ExitCode::from(2);
        }
        "upgrade" => engine::upgrade(&o.engine()),
        "recover" => engine::recover(&o.engine()),
        "update" => engine::update(&o.engine()),
        "list" => engine::list(&o.engine()),
        "search" => engine::search(&o.engine(), &o.search, &o.args),
        "info" => engine::info(&o.engine(), &o.info, &o.args),
        "fix" => engine::fix(&o.engine(), &o.args),
        "cache" => match o.args.as_slice() {
            [sub] if sub == "clean" => engine::cache_clean(&o.engine()),
            _ => {
                eprintln!("oct: usage: oct cache clean");
                return ExitCode::from(2);
            }
        },
        c => {
            eprintln!("oct: unknown command '{c}'");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    // A command that had to finish an interrupted switch first, and that
    // switch had errors: the command has not succeeded, whatever its own
    // work did (`recover` says so itself).
    let r = r.map(|ok| {
        let n = engine::recovery_errors();
        if ok && n > 0 && o.cmd != "recover" {
            println!("ERRORS: the interrupted switch that was finished first had {n} errors (they are listed above)");
        }
        ok && n == 0
    });
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        // output closed early (e.g. `| head`): not an error
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("oct: {e}");
            ExitCode::from(1)
        }
    }
}
