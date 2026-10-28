pub(super) fn unified(old: &[u8], new: &[u8], old_name: &str, new_name: &str) -> Option<String> {
    if old.len() > 1 << 20 || new.len() > 1 << 20 {
        return None;
    }
    if old == new {
        return Some(String::new());
    }
    let a = ends(old);
    let b = ends(new);
    let edits = if old.is_empty() {
        vec![b'+'; b.len() - 1]
    } else if new.is_empty() {
        vec![b'-'; a.len() - 1]
    } else {
        myers(old, new, &a, &b)?
    };
    let mut hunks: Vec<(usize, usize)> = vec![];
    for (i, e) in edits.iter().enumerate() {
        if *e == b' ' {
            continue;
        }
        let start = i.saturating_sub(3);
        let end = (i + 4).min(edits.len());
        if let Some(h) = hunks.last_mut() {
            if start <= h.1 {
                h.1 = end;
                continue;
            }
        }
        hunks.push((start, end));
    }
    let mut out = format!("--- {}\n+++ {}\n", name(old_name), name(new_name));
    let (mut pos, mut ol, mut nl) = (0, 0, 0);
    for (start, end) in hunks {
        while pos < start {
            if edits[pos] != b'+' {
                ol += 1;
            }
            if edits[pos] != b'-' {
                nl += 1;
            }
            pos += 1;
        }
        let oc = edits[start..end].iter().filter(|e| **e != b'+').count();
        let nc = edits[start..end].iter().filter(|e| **e != b'-').count();
        out += &format!("@@ -{} +{} @@\n", range(ol, oc), range(nl, nc));
        while pos < end {
            let e = edits[pos];
            let line = if e == b'+' {
                &new[b[nl]..b[nl + 1]]
            } else {
                &old[a[ol]..a[ol + 1]]
            };
            out.push(e as char);
            out += &super::utf8(line);
            if line.last() != Some(&b'\n') {
                out += "\n\\ No newline at end of file\n";
            }
            if e != b'+' {
                ol += 1;
            }
            if e != b'-' {
                nl += 1;
            }
            pos += 1;
        }
    }
    Some(out)
}
fn ends(data: &[u8]) -> Vec<usize> {
    let mut out = vec![0];
    for (i, b) in data.iter().enumerate() {
        if *b == b'\n' {
            out.push(i + 1);
        }
    }
    if out.last() != Some(&data.len()) {
        out.push(data.len());
    }
    out
}
fn range(before: usize, count: usize) -> String {
    match count {
        0 => format!("{before},0"),
        1 => (before + 1).to_string(),
        _ => format!("{},{count}", before + 1),
    }
}
fn myers(old: &[u8], new: &[u8], a: &[usize], b: &[usize]) -> Option<Vec<u8>> {
    let n = (a.len() - 1) as i32;
    let m = (b.len() - 1) as i32;
    if (n - m).abs() > 512 {
        return None;
    }
    let offset = 513i32;
    let mut v = vec![0i32; 1027];
    let mut trace = vec![];
    let mut work = 0;
    for d in 0..=512i32 {
        trace.push(v.clone());
        for k in (-d..=d).step_by(2) {
            work += 1;
            if work > 16 << 20 {
                return None;
            }
            let ix = (offset + k) as usize;
            let mut x = v[ix - 1] + 1;
            if k == -d || (k != d && v[ix - 1] < v[ix + 1]) {
                x = v[ix + 1];
            }
            let mut y = x - k;
            while x < n && y < m {
                let left = &old[a[x as usize]..a[x as usize + 1]];
                let right = &new[b[y as usize]..b[y as usize + 1]];
                work += 1;
                if left.len() == right.len() {
                    work += left.len();
                }
                if work > 16 << 20 {
                    return None;
                }
                if left != right {
                    break;
                }
                x += 1;
                y += 1;
            }
            v[ix] = x;
            if x >= n && y >= m {
                let (mut x, mut y) = (n, m);
                let mut edits = vec![];
                for d in (1..trace.len()).rev() {
                    let v = &trace[d];
                    let k = x - y;
                    let mut previous = k - 1;
                    if k == -(d as i32)
                        || (k != d as i32
                            && v[(offset + k - 1) as usize] < v[(offset + k + 1) as usize])
                    {
                        previous = k + 1;
                    }
                    let px = v[(offset + previous) as usize];
                    let py = px - previous;
                    while x > px && y > py {
                        edits.push(b' ');
                        x -= 1;
                        y -= 1;
                    }
                    if x == px {
                        edits.push(b'+');
                        y -= 1;
                    } else {
                        edits.push(b'-');
                        x -= 1;
                    }
                }
                while x > 0 && y > 0 {
                    edits.push(b' ');
                    x -= 1;
                    y -= 1;
                }
                edits.reverse();
                return Some(edits);
            }
        }
    }
    None
}
pub(super) fn name(s: &str) -> String {
    if !s
        .bytes()
        .any(|b| b < 32 || b >= 127 || b == b'\\' || b == b'"')
    {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.bytes() {
        match c {
            b'\\' | b'"' => {
                out.push('\\');
                out.push(c as char);
            }
            b'\n' => out += "\\n",
            b'\t' => out += "\\t",
            b'\r' => out += "\\r",
            0..=31 | 127..=255 => out += &format!("\\{c:03o}"),
            _ => out.push(c as char),
        }
    }
    out.push('"');
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unified_no_newline() {
        assert_eq!(unified(b"before",b"after","a/a","b/a").unwrap(),"--- a/a\n+++ b/a\n@@ -1 +1 @@\n-before\n\\ No newline at end of file\n+after\n\\ No newline at end of file\n");
    }
    #[test]
    fn unified_budget() {
        assert!(unified(&vec![b'a'; (1 << 20) + 1], b"", "a", "b").is_none());
    }
}
