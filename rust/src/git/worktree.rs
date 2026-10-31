use super::{clean, diff, pathspecs, strings, utf8, valid_oid, valid_path, Git, Output, LIMIT};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};
const CONTENT: usize = 1 << 20;
const INVENTORY: usize = 8 << 20;
#[derive(Default, Clone)]
struct File {
    mode: String,
    oid: String,
    flags: String,
    size: u64,
    content: Vec<u8>,
    note: String,
}
fn selected(p: &str, paths: &[String]) -> bool {
    valid_path(p)
        && p != "."
        && (paths.is_empty()
            || paths.iter().any(|s| {
                let s = clean(s);
                s == "." || p == s || p.starts_with(&(s + "/"))
            }))
}
fn tree(raw: &[u8]) -> Result<BTreeMap<String, File>, String> {
    let mut files = BTreeMap::new();
    for r in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let r = utf8(r);
        let (h, p) = r.split_once('\t').ok_or("invalid tree inventory")?;
        let f: Vec<_> = h.split_whitespace().collect();
        if f.len() != 3 || !valid_oid(f[2]) {
            return Err("invalid tree inventory".into());
        }
        files.insert(
            p.into(),
            File {
                mode: f[0].into(),
                oid: f[2].into(),
                ..File::default()
            },
        );
    }
    Ok(files)
}
fn index(mut raw: &[u8]) -> Result<BTreeMap<String, File>, String> {
    let mut files = BTreeMap::new();
    while !raw.is_empty() {
        let nul = raw
            .iter()
            .position(|b| *b == 0)
            .ok_or("invalid index inventory")?;
        let record = utf8(&raw[..nul]);
        let (h, p) = record.split_once('\t').ok_or("invalid index inventory")?;
        let fields: Vec<_> = h.split_whitespace().collect();
        raw = &raw[nul + 1..];
        let start = raw
            .windows(7)
            .position(|b| b == b"flags: ")
            .ok_or("invalid index inventory")?;
        if fields.len() != 3 || !valid_oid(fields[1]) {
            return Err("invalid index inventory".into());
        }
        let end = raw[start..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("invalid index flags")?
            + start;
        let flags = u32::from_str_radix(utf8(&raw[start + 7..end]).trim(), 16)
            .map_err(|_| "invalid index flags")?;
        raw = &raw[end + 1..];
        let mut notes = vec![];
        if flags & 0x40000000 != 0 {
            notes.push("skip-worktree");
        }
        if flags & 0x8000 != 0 {
            notes.push("assume-unchanged");
        }
        if flags & 0x20000000 != 0 {
            notes.push("intent-to-add");
        }
        if fields[2] != "0" {
            notes.push("unmerged index");
        }
        files.insert(
            p.into(),
            File {
                mode: fields[0].into(),
                oid: fields[1].into(),
                flags: notes.join("; "),
                ..File::default()
            },
        );
    }
    Ok(files)
}
fn change(a: &File, b: &File) -> &'static str {
    if a.mode.is_empty() && b.mode.is_empty() {
        ""
    } else if a.mode.is_empty() {
        "added"
    } else if b.mode.is_empty() {
        "deleted"
    } else if a.mode != b.mode || a.oid != b.oid {
        "modified"
    } else {
        ""
    }
}
fn mode(f: &File) -> &str {
    if f.mode.is_empty() {
        "000000"
    } else {
        &f.mode
    }
}
fn binary(f: &File) -> bool {
    f.content.contains(&0) || std::str::from_utf8(&f.content).is_err()
}
fn summary(out: &mut Output, a: &File, b: &File) {
    out.text(&format!(
        "changed: old size={} blob={}; new size={} blob={}\n",
        a.size, a.oid, b.size, b.oid
    ));
}
fn quote(p: &str) -> String {
    let mut out = String::from("\"");
    for c in p.chars() {
        match c {
            '\n' => out += "\\n",
            '\r' => out += "\\r",
            '\t' => out += "\\t",
            '\0' => out += "\\x00",
            '\\' => out += "\\\\",
            '"' => out += "\\\"",
            c if c.is_control() => {
                let n = c as u32;
                if n < 256 {
                    out += &format!("\\x{n:02x}");
                } else {
                    out += &format!("\\u{n:04x}");
                }
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
fn status(out: &mut Output, section: &str, p: &str, a: &File, b: &File, note: &str) {
    let c = change(a, b);
    if c.is_empty() {
        return;
    }
    out.text(&format!("{section} {c} {}{note}\n", quote(p)));
    if a.mode != b.mode {
        out.text(&format!("mode {} -> {}\n", mode(a), mode(b)));
    }
    out.text(&format!("blob {} -> {}\n", a.oid, b.oid));
    if a.size > CONTENT as u64 || b.size > CONTENT as u64 || binary(a) || binary(b) {
        summary(out, a, b);
    }
}

pub(super) fn compare(
    git: &Git<'_>,
    operation: &str,
    rev: &str,
    paths: &[String],
    stat: bool,
) -> Result<Output, String> {
    let mut out = Output::new();
    let format = git.run(
        &strings(&["rev-parse", "--show-object-format"]),
        None,
        LIMIT,
    )?;
    let algorithm = utf8(&format.bytes).trim().to_owned();
    if algorithm != "sha1" && algorithm != "sha256" {
        return Err("unsupported Git object format".into());
    }
    let mut args = strings(&["ls-tree", "-r", "-z", rev, "--"]);
    args.extend(if paths.is_empty() {
        vec![".".into()]
    } else {
        paths
            .iter()
            .map(|p| format!(":(literal){}", clean(p)))
            .collect()
    });
    let base_raw = git.run(&args, None, INVENTORY)?;
    let scope = pathspecs(paths);
    let mut args = strings(&["ls-files", "--stage", "--debug", "-z", "--"]);
    args.extend(scope.clone());
    let index_raw = git.run(&args, None, INVENTORY)?;
    if base_raw.seen > base_raw.bytes.len() || index_raw.seen > index_raw.bytes.len() {
        out.text("[truncated: inventory exceeds byte limit; comparison unavailable]\n");
        out.incomplete = true;
        return Ok(out);
    }
    let mut base = tree(&base_raw.bytes)?;
    let mut idx = index(&index_raw.bytes)?;
    let mut seen = BTreeSet::new();
    for p in paths {
        let p = clean(p);
        let mut parent = Path::new(&p).parent();
        while let Some(par) = parent {
            let p = par.to_string_lossy().to_string();
            if p.is_empty() || p == "." {
                break;
            }
            if seen.insert(p.clone()) {
                let raw = git.run(
                    &strings(&["ls-tree", "-z", rev, "--", &format!(":(literal){p}")]),
                    None,
                    INVENTORY,
                )?;
                if raw.seen > raw.bytes.len() {
                    return Err("gitlink ancestor inventory exceeds byte limit".into());
                }
                if let Some(f) = tree(&raw.bytes)?.remove(&p).filter(|f| f.mode == "160000") {
                    base.insert(p.clone(), f);
                }
                let glob = p
                    .replace('\\', "\\\\")
                    .replace('*', "\\*")
                    .replace('?', "\\?")
                    .replace('[', "\\[")
                    .replace(']', "\\]");
                let raw = git.run(
                    &strings(&[
                        "ls-files",
                        "--stage",
                        "--debug",
                        "-z",
                        "--",
                        &format!(":(literal){p}"),
                        &format!(":(exclude,glob){glob}/**"),
                    ]),
                    None,
                    INVENTORY,
                )?;
                if raw.seen > raw.bytes.len() {
                    return Err("index gitlink ancestor inventory exceeds byte limit".into());
                }
                if let Some(f) = index(&raw.bytes)?.remove(&p).filter(|f| f.mode == "160000") {
                    idx.insert(p, f);
                }
            }
            parent = par.parent();
        }
    }
    let mut args = strings(&["ls-files", "-z", "--others", "--exclude-standard", "--"]);
    args.extend(scope);
    let raw = git.run(&args, None, INVENTORY)?;
    let mut truncated = raw.seen > raw.bytes.len();
    let modules: BTreeSet<_> = base
        .iter()
        .chain(idx.iter())
        .filter(|(_, f)| f.mode == "160000")
        .map(|(p, _)| p.clone())
        .collect();
    let choose =
        |p: &str| selected(p, paths) && !modules.iter().any(|m| p.starts_with(&(m.clone() + "/")));
    let mut names: BTreeSet<String> = base
        .keys()
        .chain(idx.keys())
        .filter(|p| choose(p))
        .cloned()
        .collect();
    let mut untracked = BTreeSet::new();
    let bytes = if truncated {
        &raw.bytes[..raw.bytes.iter().rposition(|b| *b == 0).map_or(0, |n| n + 1)]
    } else {
        &raw.bytes
    };
    for p in bytes.split(|b| *b == 0) {
        let p = utf8(p);
        if !choose(&p) {
            continue;
        }
        if untracked.len() >= 1000 {
            truncated = true;
            break;
        }
        names.insert(p.clone());
        untracked.insert(p);
    }
    let conversions = conversions(git, &names)?;
    for p in names {
        if out.seen > LIMIT {
            break;
        }
        check_read(git).map_err(|e| e.to_string())?;
        let mut b = base.remove(&p).unwrap_or_default();
        let mut i = idx.remove(&p).unwrap_or_default();
        if operation == "status"
            && !i.flags.contains("intent-to-add")
            && !i.flags.contains("unmerged")
        {
            if !change(&b, &i).is_empty() && b.mode != "160000" && i.mode != "160000" {
                if !b.oid.is_empty() {
                    b = blob(git, b, &algorithm)?;
                }
                if !i.oid.is_empty() {
                    i = blob(git, i, &algorithm)?;
                }
            }
            status(&mut out, "staged", &p, &b, &i, "");
        }
        if !i.flags.is_empty() {
            out.text(&format!(
                "index {}: {}; comparison unknown\n",
                quote(&p),
                i.flags
            ));
            continue;
        }
        if b.mode == "160000" || i.mode == "160000" {
            out.text(&format!(
                "submodule {}: base={} index={}; working tree not inspected\n",
                quote(&p),
                b.oid,
                i.oid
            ));
            continue;
        }
        let current = working(git, &p, &algorithm);
        if !current.note.is_empty() {
            check_read(git).map_err(|e| e.to_string())?;
            out.text(&format!("unknown {}: {}\n", quote(&p), current.note));
            continue;
        }
        let conversion =
            if conversions.contains(&p) && current.mode != "120000" && !binary(&current) {
                "; comparison not filter-aware"
            } else {
                ""
            };
        if operation == "status" {
            if untracked.contains(&p) {
                out.text(&format!("untracked {}{conversion}\n", quote(&p)));
                summary(&mut out, &File::default(), &current);
            } else {
                if !i.oid.is_empty()
                    && !change(&i, &current).is_empty()
                    && (current.size > CONTENT as u64
                        || binary(&current)
                        || current.mode.is_empty())
                {
                    i = blob(git, i, &algorithm)?;
                }
                status(&mut out, "unstaged", &p, &i, &current, conversion);
            }
            continue;
        }
        let c = change(&b, &current);
        if c.is_empty() {
            continue;
        }
        out.text(&format!(
            "diff --git {} {}\n",
            diff::name(&format!("a/{p}")),
            diff::name(&format!("b/{p}"))
        ));
        out.text(&format!("{c} {}{conversion}\n", quote(&p)));
        if b.mode != current.mode {
            out.text(&format!("mode {} -> {}\n", mode(&b), mode(&current)));
        }
        if !b.oid.is_empty() {
            b = blob(git, b, &algorithm)?;
        }
        if stat {
            summary(&mut out, &b, &current);
            continue;
        }
        if b.oid == current.oid {
            continue;
        }
        if b.size > CONTENT as u64
            || current.size > CONTENT as u64
            || binary(&b)
            || binary(&current)
        {
            out.text("changed (binary or oversized)\n");
            summary(&mut out, &b, &current);
            continue;
        }
        let old = if b.mode.is_empty() {
            "/dev/null".into()
        } else {
            format!("a/{p}")
        };
        let new = if current.mode.is_empty() {
            "/dev/null".into()
        } else {
            format!("b/{p}")
        };
        match diff::unified(&b.content, &current.content, &old, &new) {
            Some(p) => out.text(&p),
            None => {
                out.text("changed (diff budget exceeded)\n");
                summary(&mut out, &b, &current);
            }
        }
    }
    if truncated {
        out.text("[truncated: untracked listing]\n");
        out.incomplete = true;
    }
    Ok(out)
}
fn conversions(git: &Git<'_>, names: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
    let mut configured = false;
    for key in ["core.autocrlf", "core.eol"] {
        let value = match git.run(&strings(&["config", "--get", key]), None, LIMIT) {
            Ok(o) => utf8(&o.bytes).trim().to_lowercase(),
            Err(e) if e == "exit:1" => String::new(),
            Err(e) => return Err(e),
        };
        if (key == "core.autocrlf" && !value.is_empty() && value != "false")
            || (key == "core.eol" && !value.is_empty() && value != "native")
        {
            configured = true;
        }
    }
    if names.is_empty() {
        return Ok(BTreeSet::new());
    }
    let input = names.iter().cloned().collect::<Vec<_>>().join("\0") + "\0";
    let output = git.run(
        &strings(&[
            "check-attr",
            "--stdin",
            "-z",
            "filter",
            "text",
            "eol",
            "crlf",
            "ident",
            "working-tree-encoding",
        ]),
        Some(input.as_bytes()),
        INVENTORY,
    )?;
    let mut result = if configured || output.seen > output.bytes.len() {
        names.clone()
    } else {
        BTreeSet::new()
    };
    let fields: Vec<_> = output.bytes.split(|b| *b == 0).collect();
    for f in fields.chunks(3) {
        if f.len() == 3 && f[2] != b"unspecified" && f[2] != b"unset" {
            result.insert(utf8(f[0]));
        }
    }
    Ok(result)
}
fn note(s: &str) -> File {
    File {
        note: s.into(),
        ..File::default()
    }
}
fn working(git: &Git<'_>, p: &str, algorithm: &str) -> File {
    let root = git.root;
    let parts: Vec<_> = p.split('/').collect();
    let mut abs = root.to_path_buf();
    for c in &parts[..parts.len() - 1] {
        abs.push(c);
        match fs::symlink_metadata(&abs) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return File::default(),
            Ok(m) if m.is_dir() => {}
            _ => return note("non-directory or symlink ancestor; not inspected"),
        }
    }
    abs = root.join(p);
    let info = match fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return File::default(),
        _ => return note("file unavailable"),
    };
    if info.file_type().is_symlink() {
        let target = match fs::read_link(&abs) {
            Ok(t) => t,
            Err(_) => return note("symlink unavailable"),
        };
        let target = target.as_os_str().as_bytes();
        let (content, oid) =
            match read_content(git, &mut &target[..], target.len() as u64, algorithm) {
                Ok(v) => v,
                Err(_) => return note("symlink changed during comparison"),
            };
        return File {
            mode: "120000".into(),
            oid,
            size: target.len() as u64,
            content,
            ..File::default()
        };
    }
    if !info.is_file() {
        return note("not a regular file or symlink; not inspected");
    }
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&abs)
    {
        Ok(f) => f,
        Err(_) => return note("file unavailable"),
    };
    let opened = match file.metadata() {
        Ok(m) if m.is_file() && m.ino() == info.ino() && m.dev() == info.dev() => m,
        _ => return note("file changed during comparison"),
    };
    // Hash the complete raw stream even when its contents exceed the display cap.
    let (bytes, oid) = match read_content(git, &mut file, opened.len(), algorithm) {
        Ok(v) => v,
        Err(_) => return note("file unreadable or changed during comparison"),
    };
    let after = match file.metadata() {
        Ok(m) => m,
        _ => return note("file changed during comparison"),
    };
    if after.len() != opened.len()
        || after.mtime() != opened.mtime()
        || after.mtime_nsec() != opened.mtime_nsec()
    {
        return note("file changed during comparison");
    }
    File {
        mode: if opened.mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        }
        .into(),
        oid,
        size: opened.len(),
        content: bytes,
        ..File::default()
    }
}
fn blob(git: &Git<'_>, mut f: File, algorithm: &str) -> Result<File, String> {
    let out = git.run(&strings(&["cat-file", "-s", &f.oid]), None, LIMIT)?;
    f.size = utf8(&out.bytes)
        .trim()
        .parse()
        .map_err(|_| "invalid blob size")?;
    if f.size > CONTENT as u64 {
        return Ok(f);
    }
    let out = git.run(&strings(&["cat-file", "blob", &f.oid]), None, CONTENT)?;
    let (content, oid) =
        read_content(git, &mut out.bytes.as_slice(), f.size, algorithm).map_err(|e| {
            match e.kind() {
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::TimedOut => e.to_string(),
                _ => "blob digest mismatch".to_owned(),
            }
        })?;
    if out.seen as u64 != f.size || oid != f.oid {
        return Err("blob digest mismatch".into());
    }
    f.content = content;
    Ok(f)
}
#[cfg(test)]
fn hash(content: &[u8], algorithm: &str) -> String {
    let mut data = format!("blob {}\0", content.len()).into_bytes();
    data.extend_from_slice(content);
    if algorithm == "sha256" {
        return format!("{:x}", Sha256::digest(&data));
    }
    sha1(&data)
}
fn check_read(git: &Git<'_>) -> std::io::Result<()> {
    if git.cancellation.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "cancelled",
        ));
    }
    if std::time::Instant::now() >= git.deadline {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"));
    }
    Ok(())
}

fn read_content(
    git: &Git<'_>,
    reader: &mut impl Read,
    size: u64,
    algorithm: &str,
) -> std::io::Result<(Vec<u8>, String)> {
    let header = format!("blob {size}\0");
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    if algorithm == "sha256" {
        sha256.update(header.as_bytes());
    } else {
        sha1.update(header.as_bytes());
    }
    let mut content = Vec::new();
    let mut seen = 0u64;
    let mut buffer = [0; 32768];
    loop {
        check_read(git)?;
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        seen += n as u64;
        if algorithm == "sha256" {
            sha256.update(&buffer[..n]);
        } else {
            sha1.update(&buffer[..n]);
        }
        if size <= CONTENT as u64 && seen <= CONTENT as u64 {
            content.extend_from_slice(&buffer[..n]);
        } else {
            content.clear();
        }
    }
    if seen != size {
        return Err(std::io::Error::other("file size changed during comparison"));
    }
    let digest = if algorithm == "sha256" {
        format!("{:x}", sha256.finalize())
    } else {
        sha1.finish()
    };
    Ok((content, digest))
}
#[cfg(test)]
fn sha1(bytes: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(bytes);
    digest.finish()
}
struct Sha1 {
    h: [u32; 5],
    tail: Vec<u8>,
    seen: u64,
}
impl Sha1 {
    fn new() -> Self {
        Self {
            h: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0],
            tail: Vec::new(),
            seen: 0,
        }
    }
    fn update(&mut self, mut bytes: &[u8]) {
        self.seen += bytes.len() as u64;
        if !self.tail.is_empty() {
            let n = (64 - self.tail.len()).min(bytes.len());
            self.tail.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.tail.len() == 64 {
                let block = self.tail.clone();
                self.block(&block);
                self.tail.clear();
            }
        }
        while bytes.len() >= 64 {
            self.block(&bytes[..64]);
            bytes = &bytes[64..];
        }
        self.tail.extend_from_slice(bytes);
    }
    fn finish(mut self) -> String {
        let bits = self.seen * 8;
        let mut padding = vec![0x80];
        while (self.tail.len() + padding.len()) % 64 != 56 {
            padding.push(0);
        }
        padding.extend_from_slice(&bits.to_be_bytes());
        self.update(&padding);
        self.h.iter().map(|x| format!("{x:08x}")).collect()
    }
    fn block(&mut self, block: &[u8]) {
        let h = &mut self.h;
        let mut w = [0u32; 80];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(c.try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = *h;
        for (i, v) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*v);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (i, v) in [a, b, c, d, e].iter().enumerate() {
            h[i] = h[i].wrapping_add(*v);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{atomic::Ordering, Arc},
        time::{Duration, Instant},
    };

    struct InterruptedReader {
        first: bool,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
        cancel: bool,
    }
    impl Read for InterruptedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if !self.first {
                panic!("raw hashing read another chunk after interruption");
            }
            self.first = false;
            buffer.fill(b'x');
            if self.cancel {
                self.cancellation
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            Ok(buffer.len())
        }
    }

    #[test]
    fn raw_hash_checks_cancellation_and_deadline_between_chunks() {
        for algorithm in ["sha1", "sha256"] {
            for cancel in [true, false] {
                let git = Git {
                    root: Path::new("."),
                    deadline: Instant::now()
                        + if cancel {
                            Duration::from_secs(10)
                        } else {
                            Duration::from_millis(10)
                        },
                    cancellation: Arc::default(),
                };
                let mut reader = InterruptedReader {
                    first: true,
                    cancellation: git.cancellation.clone(),
                    cancel,
                };
                let error =
                    read_content(&git, &mut reader, CONTENT as u64 + 1, algorithm).unwrap_err();
                assert!(
                    !reader.first,
                    "must read the first chunk before interruption"
                );
                assert_eq!(
                    error.kind(),
                    if cancel {
                        std::io::ErrorKind::Interrupted
                    } else {
                        std::io::ErrorKind::TimedOut
                    }
                );
            }
        }
    }

    #[test]
    fn working_hash_preserves_complete_digests_and_interruption_notes() {
        let directory = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("target", directory.path().join("link")).unwrap();
        for algorithm in ["sha1", "sha256"] {
            for size in [6, CONTENT + 1] {
                let bytes = vec![b'x'; size];
                fs::write(directory.path().join("file"), &bytes).unwrap();
                let mut git = Git {
                    root: directory.path(),
                    deadline: Instant::now() + Duration::from_secs(10),
                    cancellation: Arc::default(),
                };
                let file = working(&git, "file", algorithm);
                assert!(file.note.is_empty());
                assert_eq!(file.size, size as u64);
                assert_eq!(file.oid, hash(&bytes, algorithm));
                assert_eq!(file.content, if size <= CONTENT { bytes } else { vec![] });
                let link = working(&git, "link", algorithm);
                assert_eq!(link.oid, hash(b"target", algorithm));
                assert_eq!(link.mode, "120000");

                for cancel in [true, false] {
                    git.cancellation.store(cancel, Ordering::SeqCst);
                    if !cancel {
                        git.deadline = Instant::now();
                    }
                    for (path, expected) in [
                        ("file", "file unreadable or changed during comparison"),
                        ("link", "symlink changed during comparison"),
                    ] {
                        let file = working(&git, path, algorithm);
                        assert_eq!(file.note, expected);
                        assert!(file.oid.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn known_blob_hash() {
        assert_eq!(
            hash(b"hello\n", "sha1"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }
    #[test]
    fn index_flags_are_not_assumed_clean() {
        let raw=b"100644 ce013625030ba8dba906f756967f9e9ca394464a 0\ta\0  ctime: 0:0\n  mtime: 0:0\n  dev: 0 ino: 0\n  uid: 0 gid: 0\n  size: 0 flags: 40008000\n";
        assert_eq!(
            index(raw).unwrap()["a"].flags,
            "skip-worktree; assume-unchanged"
        );
    }
}
