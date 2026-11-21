use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use blk::{Seq, Wc};

mod alloc;
mod blk;
mod fast;
mod out;
mod pq;
mod scan;

#[global_allocator]
static GA: alloc::Huge = alloc::Huge;

const DAY_MS: i64 = 86_400_000;
const GAP_MS: u32 = 1_800_000;
const WEEK_MS: u32 = 604_800_000;
const LATE_MS: i64 = 86_400_000;
const P1_BITS: u32 = 10;
const P3_BITS: u32 = 10;
const CHUNK: usize = 2 << 20;
const FB: u64 = 1 << 63;
const INVALID: u32 = u32::MAX;

static INTERN: Mutex<Option<(HashMap<Vec<u8>, u64>, Vec<Vec<u8>>)>> = Mutex::new(None);

fn intern(s: &[u8]) -> u64 {
    let mut g = INTERN.lock().unwrap();
    let (m, v) = g.get_or_insert_with(|| (HashMap::new(), Vec::new()));
    if let Some(&k) = m.get(s) {
        return k;
    }
    let k = FB | v.len() as u64;
    v.push(s.to_vec());
    m.insert(s.to_vec(), k);
    k
}

fn interned() -> Vec<Vec<u8>> {
    INTERN.lock().unwrap().as_ref().map(|x| x.1.clone()).unwrap_or_default()
}

const C64: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let c = i as u8;
        t[i] = match c {
            b'0'..=b'9' => c - b'0' + 1,
            b'A'..=b'Z' => c - b'A' + 11,
            b'_' => 37,
            b'a'..=b'z' => c - b'a' + 38,
            _ => 0,
        };
        i += 1;
    }
    t
};

const D64: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 256 {
        let c = C64[i];
        if c != 0 {
            t[c as usize] = i as u8;
        }
        i += 1;
    }
    t
};

#[inline]
fn enc(s: &[u8]) -> u64 {
    let n = s.len();
    if n.wrapping_sub(1) < 10 {
        let mut v = 0u64;
        let mut bad = false;
        for &b in s {
            let c = C64[b as usize];
            bad |= c == 0;
            v = (v << 6) | c as u64;
        }
        if !bad {
            return v << (6 * (10 - n));
        }
    }
    intern(s)
}

#[inline(always)]
fn fts(end: usize, f: &[u8]) -> Option<i64> {
    if f.as_ptr() as usize + fast::PAD <= end {
        unsafe { fast::ts_p(f.as_ptr(), f.len()) }
    } else {
        parse_ts(f)
    }
}

#[inline(always)]
fn fenc(end: usize, f: &[u8]) -> u64 {
    if f.as_ptr() as usize + fast::PAD <= end {
        match unsafe { fast::enc_p(f.as_ptr(), f.len()) } {
            Some(v) => v,
            None => intern(f),
        }
    } else {
        enc(f)
    }
}

pub fn dec_into(k: u64, tab: &[Vec<u8>], out: &mut Vec<u8>) {
    if k & FB != 0 {
        out.extend_from_slice(&tab[(k & !FB) as usize]);
        return;
    }
    for i in (0..10).rev() {
        let c = ((k >> (6 * i)) & 63) as usize;
        if c == 0 {
            break;
        }
        out.push(D64[c]);
    }
}

fn dec(k: u64, tab: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::with_capacity(12);
    dec_into(k, tab, &mut v);
    v
}

fn kless(a: u64, b: u64, tab: &[Vec<u8>]) -> bool {
    if (a | b) & FB == 0 {
        a < b
    } else {
        dec(a, tab) < dec(b, tab)
    }
}

#[inline]
fn mix(k: u64) -> u64 {
    let mut x = k ^ (k >> 31);
    x = x.wrapping_mul(0x7fb5d329728ea185);
    x ^= x >> 27;
    x = x.wrapping_mul(0x81dadef4bc2dd44d);
    x ^ (x >> 33)
}

struct Table {
    kv: Vec<u64>,
    mask: usize,
}
impl Table {
    fn new(n: usize) -> Table {
        let cap = (n * 2).next_power_of_two().max(16);
        Table { kv: vec![0; cap * 2], mask: cap - 1 }
    }
    fn insert(&mut self, k: u64, v: u64) {
        let mut i = mix(k) as usize & self.mask;
        loop {
            let s = self.kv[2 * i];
            if s == 0 || s == k {
                self.kv[2 * i] = k;
                self.kv[2 * i + 1] = v;
                return;
            }
            i = (i + 1) & self.mask;
        }
    }
    #[inline]
    fn get(&self, k: u64) -> Option<u64> {
        let mut i = mix(k) as usize & self.mask;
        loop {
            let s = unsafe { *self.kv.get_unchecked(2 * i) };
            if s == k {
                return Some(unsafe { *self.kv.get_unchecked(2 * i + 1) });
            }
            if s == 0 {
                return None;
            }
            i = (i + 1) & self.mask;
        }
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil(dd: i64) -> (i64, i64, i64) {
    let z = dd + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[inline]
fn d2(s: &[u8], i: usize) -> Option<i64> {
    let a = s[i].wrapping_sub(b'0');
    let b = s[i + 1].wrapping_sub(b'0');
    if a > 9 || b > 9 {
        return None;
    }
    Some(a as i64 * 10 + b as i64)
}

#[inline]
fn parse_ts(s: &[u8]) -> Option<i64> {
    let off_ms = match s.len() {
        20 if s[19] == b'Z' => 0,
        25 if (s[19] == b'+' || s[19] == b'-') && s[22] == b':' => {
            let oh = d2(s, 20)?;
            let om = d2(s, 23)?;
            if oh > 23 || om > 59 {
                return None;
            }
            let o = (oh * 3600 + om * 60) * 1000;
            if s[19] == b'+' { o } else { -o }
        }
        0 => return None,
        _ => return parse_f3(s),
    };
    if s[4] != b'-' || s[7] != b'-' || s[10] != b'T' || s[13] != b':' || s[16] != b':' {
        return None;
    }
    let y = d2(s, 0)? * 100 + d2(s, 2)?;
    let mo = d2(s, 5)?;
    let d = d2(s, 8)?;
    let h = d2(s, 11)?;
    let mi = d2(s, 14)?;
    let se = d2(s, 17)?;
    if !(1..=12).contains(&mo) || d < 1 || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let md = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(mo - 1) as usize];
    if d > md {
        return None;
    }
    Some((days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se) * 1000 - off_ms)
}

#[inline]
fn parse_f3(s: &[u8]) -> Option<i64> {
    let mut v: u64 = 0;
    let mut sig = 0;
    for &b in s {
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        if sig > 0 || d != 0 {
            sig += 1;
            if sig <= 19 {
                v = v * 10 + d as u64;
            }
        }
    }
    if sig > 19 || v > i64::MAX as u64 {
        return Some(i64::MAX);
    }
    Some(v as i64)
}

#[inline]
fn row<'a, const N: usize>(line: &'a [u8], out: &mut [&'a [u8]; N]) -> bool {
    let mut k = 0;
    let mut start = 0;
    let n = line.len();
    let mut i = 0;
    let mut q = 0u32;
    while i <= n {
        let b = if i < n { line[i] } else { b',' };
        if b == b',' {
            if k == N {
                return false;
            }
            let f = &line[start..i];
            out[k] = if q == 0 {
                f
            } else if q == 2 && f.len() >= 2 && f[0] == b'"' && f[f.len() - 1] == b'"' {
                &f[1..f.len() - 1]
            } else {
                return false;
            };
            k += 1;
            q = 0;
            start = i + 1;
        } else if b == b'"' {
            q += 1;
        }
        i += 1;
    }
    k == N && n > 0
}

struct Mapped(*const u8, usize);
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}
impl Mapped {
    fn bytes(&self) -> &[u8] {
        if self.1 == 0 { &[] } else { unsafe { std::slice::from_raw_parts(self.0, self.1) } }
    }
}

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

fn map_dir(dir: &std::path::Path) -> Vec<Mapped> {
    use std::os::unix::io::AsRawFd;
    let mut ps: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "csv") && p.is_file()).collect())
        .unwrap_or_else(|_| Vec::new());
    ps.sort();
    ps.iter()
        .map(|p| {
            let f = std::fs::File::open(p).unwrap();
            let len = f.metadata().unwrap().len() as usize;
            if len == 0 {
                Mapped(std::ptr::null(), 0)
            } else {
                Mapped(unsafe { mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0) }, len)
            }
        })
        .collect()
}

fn chunks(files: &[Mapped], tag: u8, out: &mut Vec<(u8, usize, usize, usize)>) {
    for (fi, m) in files.iter().enumerate() {
        let b = m.bytes();
        let mut s = match memchr(b'\n', b) {
            Some(p) => p + 1,
            None => continue,
        };
        while s < b.len() {
            let mut e = (s + CHUNK).min(b.len());
            if e < b.len() {
                e = memchr(b'\n', &b[e..]).map_or(b.len(), |p| e + p + 1);
            }
            out.push((tag, fi, s, e));
            s = e;
        }
    }
}

#[inline]
fn memchr(c: u8, b: &[u8]) -> Option<usize> {
    b.iter().position(|&x| x == c)
}

#[inline]
fn lines<'a, F: FnMut(&'a [u8])>(b: &'a [u8], mut f: F) {
    let mut s = 0;
    while s < b.len() {
        let e = memchr(b'\n', &b[s..]).map_or(b.len(), |p| s + p);
        f(&b[s..e]);
        s = e + 1;
    }
}

fn rsort<T: Copy, K: Fn(&T) -> u32>(a: &mut Vec<T>, b: &mut Vec<T>, key: K) {
    let mut h = [[0u32; 2048]; 2];
    for r in a.iter() {
        let k = key(r);
        h[0][((k >> 10) & 2047) as usize] += 1;
        h[1][(k >> 21) as usize] += 1;
    }
    b.clear();
    b.reserve(a.len());
    unsafe { b.set_len(a.len()) };
    for p in 0..2 {
        let mut o = 0u32;
        for x in h[p].iter_mut() {
            let c = *x;
            *x = o;
            o += c;
        }
        let sh = 10 + p as u32 * 11;
        for r in a.iter() {
            let d = ((key(r) >> sh) & 2047) as usize;
            unsafe { *b.get_unchecked_mut(h[p][d] as usize) = *r };
            h[p][d] += 1;
        }
        std::mem::swap(a, b);
    }
}

fn isort<T: Copy, K: Fn(&T) -> u32>(a: &mut [T], key: K) {
    for i in 1..a.len() {
        let x = a[i];
        let k = key(&x);
        let mut j = i;
        while j > 0 && key(&a[j - 1]) > k {
            a[j] = a[j - 1];
            j -= 1;
        }
        a[j] = x;
    }
}

fn run<T: Send, F: Fn(usize) -> T + Sync>(nt: usize, f: F) -> Vec<T> {
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..nt).map(|t| { let f = &f; sc.spawn(move || f(t)) }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn small_csv(path: &std::path::Path) -> Vec<[Vec<u8>; 2]> {
    let b = std::fs::read(path).unwrap_or_default();
    let mut v = Vec::new();
    if let Some(p) = memchr(b'\n', &b) {
        let mut f: [&[u8]; 2] = [&[]; 2];
        lines(&b[p + 1..], |l| {
            if row(l, &mut f) {
                v.push([f[0].to_vec(), f[1].to_vec()]);
            }
        });
    }
    v
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct ERec {
    eid: u64,
    person: u64,
    ingest: i32,
    ts: u32,
    meta: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct PRec {
    person: u64,
    ts: u32,
    meta: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CRec {
    person: u64,
    cid: u64,
    ts: u32,
    camp: u32,
}

fn json_str(text: &str, key: &str) -> String {
    let i = text.find(&format!("\"{}\"", key)).unwrap();
    let rest = &text[i + key.len() + 2..];
    let a = rest.find('"').unwrap();
    let r2 = &rest[a + 1..];
    r2[..r2.find('"').unwrap()].to_string()
}

fn find(p: &[AtomicU32], mut x: u32) -> u32 {
    loop {
        let px = p[x as usize].load(Relaxed);
        if px == x {
            return x;
        }
        let gx = p[px as usize].load(Relaxed);
        if gx != px {
            p[x as usize].store(gx, Relaxed);
        }
        x = gx;
    }
}

pub struct UserRows {
    pub uid: Vec<u8>,
    pub uoff: Vec<u32>,
    pub day: Vec<i32>,
    pub sessions: Vec<i64>,
    pub events: Vec<i64>,
    pub revenue: Vec<i64>,
    pub top: Vec<u8>,
}

fn main() {
    let t0 = std::time::Instant::now();
    let tr = std::env::var("SS_T").is_ok();
    let lap = |n: &str| {
        if tr {
            eprintln!("{:>8.1}ms {}", t0.elapsed().as_secs_f64() * 1e3, n)
        }
    };
    let data = std::path::PathBuf::from(std::env::var("DATA_DIR").unwrap_or("/data".into()));
    let outdir = std::path::PathBuf::from(std::env::var("OUTPUT_DIR").unwrap_or("/output".into()));
    let nt: usize = std::env::var("NCPU").ok().and_then(|x| x.parse().ok()).unwrap_or(8).max(1);

    let man = std::fs::read_to_string(data.join("manifest.json")).unwrap();
    let wstart = parse_ts(json_str(&man, "window_start").as_bytes()).unwrap();
    let wend = parse_ts(json_str(&man, "window_end").as_bytes()).unwrap();
    let base = wstart - WEEK_MS as i64;
    let day0 = wstart.div_euclid(DAY_MS);
    let ndays = ((wend - 1).div_euclid(DAY_MS) - day0 + 1).max(1) as usize;

    let prows = small_csv(&data.join("products.csv"));
    let mut catnames: Vec<Vec<u8>> = prows.iter().map(|r| r[1].clone()).collect();
    catnames.push(b"unknown".to_vec());
    catnames.sort();
    catnames.dedup();
    assert!(catnames.len() <= 128);
    let unknown = catnames.iter().position(|c| c == b"unknown").unwrap() as u64;
    let mut prod = Table::new(prows.len());
    for r in &prows {
        prod.insert(enc(&r[0]), catnames.binary_search(&r[1]).unwrap() as u64);
    }
    let crows = small_csv(&data.join("campaigns.csv"));
    let mut camps = Table::new(crows.len());
    let mut cname: Vec<Vec<u8>> = Vec::new();
    let mut cchan: Vec<Vec<u8>> = Vec::new();
    for r in &crows {
        let k = enc(&r[0]);
        if camps.get(k).is_none() {
            camps.insert(k, cname.len() as u64);
            cname.push(r[0].clone());
            cchan.push(r[1].clone());
        }
    }
    let organic = cname.len() as u32;
    cname.push(b"organic".to_vec());
    cchan.push(b"organic".to_vec());
    let ncamp = cname.len();
    lap("small");

    let lfiles = map_dir(&data.join("links"));
    let efiles = map_dir(&data.join("events"));
    let cfiles = map_dir(&data.join("clicks"));
    let lbytes: usize = lfiles.iter().map(|m| m.1).sum();
    let mut lch = Vec::new();
    chunks(&lfiles, 0, &mut lch);
    let cap = (lbytes / 10 + 64).next_power_of_two();
    let mask = cap - 1;
    let lkeys: Vec<AtomicU64> = (0..cap).map(|_| AtomicU64::new(0)).collect();
    let parent: Vec<AtomicU32> = (0..cap as u32).map(AtomicU32::new).collect();
    let slot = |k: u64| -> u32 {
        let mut i = mix(k) as usize & mask;
        loop {
            let s = lkeys[i].load(Relaxed);
            if s == k {
                return i as u32;
            }
            if s == 0 {
                match lkeys[i].compare_exchange(0, k, Relaxed, Relaxed) {
                    Ok(_) => return i as u32,
                    Err(x) if x == k => return i as u32,
                    Err(_) => continue,
                }
            }
            i = (i + 1) & mask;
        }
    };
    let next = AtomicUsize::new(0);
    run(nt, |_| {
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= lch.len() {
                break;
            }
            let (_, fi, s, e) = lch[i];
            scan::tokenize::<2, _>(&lfiles[fi].bytes()[s..e], |f| {
                if !f[0].is_empty() && !f[1].is_empty() {
                    let a = slot(enc(f[0]));
                    let b = slot(enc(f[1]));
                    loop {
                        let ra = find(&parent, a);
                        let rb = find(&parent, b);
                        if ra == rb {
                            break;
                        }
                        let (hi, lo) = if ra > rb { (ra, rb) } else { (rb, ra) };
                        if parent[hi as usize].compare_exchange(hi, lo, Relaxed, Relaxed).is_ok() {
                            break;
                        }
                    }
                }
            });
        }
    });
    lap("links uf");
    let name: Vec<AtomicU64> = (0..cap).map(|_| AtomicU64::new(u64::MAX)).collect();
    let per = cap.div_ceil(nt);
    let fbs: Vec<Vec<u32>> = run(nt, |t| {
        let mut fb = Vec::new();
        for i in t * per..((t + 1) * per).min(cap) {
            let k = lkeys[i].load(Relaxed);
            if k == 0 {
                continue;
            }
            if k & FB != 0 {
                fb.push(i as u32);
            } else {
                name[find(&parent, i as u32) as usize].fetch_min(k, Relaxed);
            }
        }
        fb
    });
    let tab0 = interned();
    for fb in &fbs {
        for &i in fb {
            let k = lkeys[i as usize].load(Relaxed);
            let r = find(&parent, i) as usize;
            let cur = name[r].load(Relaxed);
            if cur == u64::MAX || kless(k, cur, &tab0) {
                name[r].store(k, Relaxed);
            }
        }
    }
    let np3 = 1usize << P3_BITS;
    let lparts: Vec<Vec<Vec<(u64, u64)>>> = run(nt, |t| {
        let mut v: Vec<Vec<(u64, u64)>> = (0..np3).map(|_| Vec::new()).collect();
        for i in t * per..((t + 1) * per).min(cap) {
            let k = lkeys[i].load(Relaxed);
            if k != 0 {
                v[(mix(k) >> (64 - P3_BITS)) as usize].push((k, name[find(&parent, i as u32) as usize].load(Relaxed)));
            }
        }
        v
    });
    drop(name);
    drop(parent);
    drop(lkeys);
    lap("links map");

    let mut ch = Vec::new();
    chunks(&efiles, 0, &mut ch);
    chunks(&cfiles, 1, &mut ch);
    let np1 = 1usize << P1_BITS;
    let next = AtomicUsize::new(0);
    let parsed: Vec<(Vec<Seq<ERec>>, Vec<Seq<CRec>>)> = run(nt, |_| {
        let mut ep: Vec<Seq<ERec>> = (0..np1).map(|_| Seq::new()).collect();
        let mut ew = Wc::new(np1, ERec { eid: 0, person: 0, ingest: 0, ts: 0, meta: 0 });
        let mut cp: Vec<Seq<CRec>> = (0..np3).map(|_| Seq::new()).collect();
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= ch.len() {
                break;
            }
            let (tag, fi, s, e) = ch[i];
            let whole = if tag == 0 { efiles[fi].bytes() } else { cfiles[fi].bytes() };
            let fb = &whole[s..e];
            let end = whole.as_ptr() as usize + whole.len();
            if tag == 0 {
                scan::tokenize::<7, _>(fb, |f| {
                    if f[0].is_empty() {
                        return;
                    }
                    let eid = fenc(end, f[0]);
                    let g = f[3];
                    let mut r = ERec { eid, person: 0, ingest: i32::MAX, ts: INVALID, meta: 0 };
                    let ing = if g.len() == 20 && g[19] == b'Z' {
                        let sec = unsafe { fast::ing_p(g.as_ptr()) };
                        r.ingest = (sec - wstart / 1000).clamp(i32::MIN as i64, i32::MAX as i64 - 1) as i32;
                        Some(sec * 1000)
                    } else {
                        let ing = fts(end, g);
                        if let Some(ig) = ing {
                            if ig != i64::MAX {
                                r.ingest = (ig.div_euclid(1000) - wstart / 1000).clamp(i32::MIN as i64, i32::MAX as i64 - 1) as i32;
                            }
                        }
                        ing
                    };
                    let typ = if f[4].as_ptr() as usize + fast::PAD <= end {
                        unsafe { fast::etype_p(f[4].as_ptr(), f[4].len()) }
                    } else {
                        match f[4] {
                            b"view" => 1u8,
                            b"cart" => 2,
                            b"purchase" => 3,
                            _ => 0,
                        }
                    };
                    if typ != 0 && !f[1].is_empty() && !f[5].is_empty() && r.ingest != i32::MAX {
                        if let (Some(t), Some(ig)) = (fts(end, f[2]), ing) {
                            if t >= wstart && t < wend && ig - t <= LATE_MS {
                                let mut meta = 0u32;
                                let mut ok = true;
                                if typ == 3 {
                                    let p = f[6];
                                    let v = if p.as_ptr() as usize + fast::PAD <= end {
                                        unsafe { fast::price_p(p.as_ptr(), p.len()) }
                                    } else {
                                        fast::price_slow(p)
                                    };
                                    ok = v.is_some();
                                    if let Some(v) = v {
                                        let cat = prod.get(fenc(end, f[5])).unwrap_or(unknown) as u32;
                                        meta = (1 << 31) | (cat << 24) | v;
                                    }
                                }
                                if ok {
                                    r.ts = (t - base) as u32;
                                    r.meta = meta;
                                    r.person = fenc(end, f[1]);
                                }
                            }
                        }
                    }
                    ew.push((mix(eid) >> (64 - P1_BITS)) as usize, r, &mut ep);
                });
            } else {
                scan::tokenize::<4, _>(fb, |f4| {
                    if f4[0].is_empty() || f4[1].is_empty() {
                        return;
                    }
                    let t = match fts(end, f4[3]) {
                        Some(t) if t >= base && t < wend => (t - base) as u32,
                        _ => return,
                    };
                    let camp = match camps.get(fenc(end, f4[2])) {
                        Some(c) => c as u32,
                        None => return,
                    };
                    let u = fenc(end, f4[1]);
                    cp[(mix(u) >> (64 - P3_BITS)) as usize].push(CRec { person: u, cid: fenc(end, f4[0]), ts: t, camp });
                });
            }
        }
        ew.flush(&mut ep);
        (ep, cp)
    });
    let mut ept: Vec<Mutex<Vec<Seq<ERec>>>> = (0..np1).map(|_| Mutex::new(Vec::new())).collect();
    let mut cpt: Vec<Mutex<Vec<Seq<CRec>>>> = (0..np3).map(|_| Mutex::new(Vec::new())).collect();
    for (ep, cp) in parsed {
        for (p, x) in ep.into_iter().enumerate() {
            ept[p].get_mut().unwrap().push(x);
        }
        for (u, x) in cp.into_iter().enumerate() {
            cpt[u].get_mut().unwrap().push(x);
        }
    }
    lap("parse");

    let next = AtomicUsize::new(0);
    let pparts: Vec<Vec<Seq<PRec>>> = run(nt, |_| {
        let mut out: Vec<Seq<PRec>> = (0..np3).map(|_| Seq::new()).collect();
        let mut flat: Vec<ERec> = Vec::new();
        let mut ix: Vec<u32> = Vec::new();
        loop {
            let p = next.fetch_add(1, Relaxed);
            if p >= np1 {
                break;
            }
            let parts = std::mem::take(&mut *ept[p].lock().unwrap());
            flat.clear();
            for x in parts.iter() {
                for c in x.chunks() {
                    flat.extend_from_slice(c);
                }
            }
            drop(parts);
            let cap = (flat.len() * 2).next_power_of_two().max(16);
            let m = cap - 1;
            ix.clear();
            ix.resize(cap, u32::MAX);
            for k in 0..flat.len() {
                let eid = flat[k].eid;
                let mut i = (mix(eid) as usize) & m;
                loop {
                    let s = ix[i];
                    if s == u32::MAX {
                        ix[i] = k as u32;
                        break;
                    }
                    let s = s as usize;
                    if flat[s].eid == eid {
                        if flat[k].ingest < flat[s].ingest {
                            ix[i] = k as u32;
                            flat[s].ts = INVALID;
                        } else {
                            flat[k].ts = INVALID;
                        }
                        break;
                    }
                    i = (i + 1) & m;
                }
            }
            for r in flat.iter() {
                if r.ts != INVALID {
                    out[(mix(r.person) >> (64 - P3_BITS)) as usize].push(PRec { person: r.person, ts: r.ts, meta: r.meta });
                }
            }
        }
        out
    });
    let mut ppt: Vec<Mutex<Vec<Seq<PRec>>>> = (0..np3).map(|_| Mutex::new(Vec::new())).collect();
    for pp in pparts {
        for (u, x) in pp.into_iter().enumerate() {
            ppt[u].get_mut().unwrap().push(x);
        }
    }
    lap("dedup");
    let next = AtomicUsize::new(0);
    let resolved: Vec<(Vec<Seq<PRec>>, Vec<Seq<CRec>>)> = run(nt, |_| {
        let mut eo: Vec<Seq<PRec>> = (0..np3).map(|_| Seq::new()).collect();
        let mut co: Vec<Seq<CRec>> = (0..np3).map(|_| Seq::new()).collect();
        let mut tb = Table { kv: Vec::new(), mask: 0 };
        loop {
            let u = next.fetch_add(1, Relaxed);
            if u >= np3 {
                break;
            }
            let n: usize = lparts.iter().map(|x| x[u].len()).sum();
            let c = (n * 2).next_power_of_two().max(16);
            tb.kv.clear();
            tb.kv.resize(2 * c, 0);
            tb.mask = c - 1;
            for x in &lparts {
                for &(k, v) in &x[u] {
                    tb.insert(k, v);
                }
            }
            let ps = std::mem::take(&mut *ppt[u].lock().unwrap());
            let cs = std::mem::take(&mut *cpt[u].lock().unwrap());
            for x in &ps {
                for r in x.iter() {
                    let mut r = *r;
                    r.person = tb.get(r.person).unwrap_or(r.person);
                    eo[(mix(r.person) >> (64 - P3_BITS)) as usize].push(r);
                }
            }
            drop(ps);
            for x in &cs {
                for r in x.iter() {
                    let mut r = *r;
                    r.person = tb.get(r.person).unwrap_or(r.person);
                    co[(mix(r.person) >> (64 - P3_BITS)) as usize].push(r);
                }
            }
        }
        (eo, co)
    });
    let mut rt: Vec<Mutex<(Vec<Seq<PRec>>, Vec<Seq<CRec>>)>> = (0..np3).map(|_| Mutex::new((Vec::new(), Vec::new()))).collect();
    for (eo, co) in resolved {
        for (q, (a, b)) in eo.into_iter().zip(co).enumerate() {
            let r = rt[q].get_mut().unwrap();
            r.0.push(a);
            r.1.push(b);
        }
    }
    lap("resolve");

    let tab = interned();
    let ncat = catnames.len();
    let mut order: Vec<usize> = (0..np3).collect();
    let sizes: Vec<usize> = rt.iter_mut().map(|r| r.get_mut().unwrap().0.iter().map(|x| x.len()).sum()).collect();
    order.sort_by_key(|&q| std::cmp::Reverse(sizes[q]));
    let ntot: usize = sizes.iter().sum();
    let next = AtomicUsize::new(0);
    let results: Vec<(UserRows, Vec<[u32; 2]>, Vec<u64>, Vec<u32>)> = run(nt, |_| {
        let mut ur = UserRows { uid: Vec::new(), uoff: vec![0], day: Vec::new(), sessions: Vec::new(), events: Vec::new(), revenue: Vec::new(), top: Vec::new() };
        let z = ntot / nt + 65536;
        ur.uid.reserve(z * 12);
        ur.uoff.reserve(z);
        ur.day.reserve(z);
        ur.sessions.reserve(z);
        ur.events.reserve(z);
        ur.revenue.reserve(z);
        ur.top.reserve(z);
        let mut camp = vec![[0u32; 2]; ncamp * ndays];
        let mut casst = vec![0u32; ncamp * ndays];
        let mut crev = vec![0u64; ncamp * ndays];
        let mut ev: Vec<(u32, u32)> = Vec::new();
        let mut cl: Vec<CRec> = Vec::new();
        let mut catrev = vec![0u64; ncat];
        let mut starts: Vec<u32> = Vec::new();
        let mut ad: Vec<(u32, u32)> = Vec::new();
        let mut sdi: Vec<u32> = Vec::new();
        let mut lo: Vec<u32> = vec![0; ncamp];
        let mut hi: Vec<u32> = vec![0; ncamp];
        let mut open: Vec<bool> = vec![false; ncamp];
        let mut touched: Vec<u32> = Vec::new();
        let mut name: Vec<u8> = Vec::new();
        let mut raw: Vec<PRec> = Vec::new();
        let mut craw: Vec<CRec> = Vec::new();
        let mut rtmp: Vec<PRec> = Vec::new();
        let mut ctmp: Vec<CRec> = Vec::new();
        let mut gt: Vec<(u64, u32, u32)> = Vec::new();
        let mut eslot: Vec<u32> = Vec::new();
        let mut cslot: Vec<u32> = Vec::new();
        let mut groups: Vec<(u64, u32, u32, u32, u32)> = Vec::new();
        loop {
            let k = next.fetch_add(1, Relaxed);
            if k >= np3 {
                break;
            }
            let q = order[k];
            raw.clear();
            craw.clear();
            let (es, cs) = std::mem::take(&mut *rt[q].lock().unwrap());
            for x in &es {
                for c in x.chunks() {
                    raw.extend_from_slice(c);
                }
            }
            for x in &cs {
                for c in x.chunks() {
                    craw.extend_from_slice(c);
                }
            }
            drop((es, cs));
            rsort(&mut raw, &mut rtmp, |r| r.ts);
            rsort(&mut craw, &mut ctmp, |r| r.ts);
            let mut cap = (raw.len() / 8 + 16).next_power_of_two();
            'g: loop {
                let m = cap - 1;
                gt.clear();
                gt.resize(cap, (0, 0, 0));
                eslot.clear();
                let mut np = 0usize;
                for r in &raw {
                    let mut h = mix(r.person) as usize & m;
                    loop {
                        let k = gt[h].0;
                        if k == r.person {
                            break;
                        }
                        if k == 0 {
                            gt[h].0 = r.person;
                            np += 1;
                            if np * 2 > cap {
                                cap *= 4;
                                continue 'g;
                            }
                            break;
                        }
                        h = (h + 1) & m;
                    }
                    gt[h].1 += 1;
                    eslot.push(h as u32);
                }
                break;
            }
            let m = cap - 1;
            cslot.clear();
            for r in &craw {
                let mut h = mix(r.person) as usize & m;
                loop {
                    let k = gt[h].0;
                    if k == r.person {
                        gt[h].2 += 1;
                        cslot.push(h as u32);
                        break;
                    }
                    if k == 0 {
                        cslot.push(u32::MAX);
                        break;
                    }
                    h = (h + 1) & m;
                }
            }
            groups.clear();
            let (mut eo, mut co) = (0u32, 0u32);
            for g in gt.iter_mut() {
                if g.0 != 0 {
                    let (ne, nc) = (g.1, g.2);
                    groups.push((g.0, eo, ne, co, nc));
                    g.1 = eo;
                    g.2 = co;
                    eo += ne;
                    co += nc;
                }
            }
            ev.clear();
            ev.resize(raw.len(), (0, 0));
            for (r, &h) in raw.iter().zip(eslot.iter()) {
                let o = &mut gt[h as usize].1;
                ev[*o as usize] = (r.ts, r.meta);
                *o += 1;
            }
            cl.clear();
            cl.resize(co as usize, CRec { person: 0, cid: 0, ts: 0, camp: 0 });
            for (r, &h) in craw.iter().zip(cslot.iter()) {
                if h != u32::MAX {
                    let o = &mut gt[h as usize].2;
                    cl[*o as usize] = *r;
                    *o += 1;
                }
            }
            for &(_, eo, ne, co, nc) in groups.iter() {
                isort(&mut ev[eo as usize..(eo + ne) as usize], |r| r.0);
                isort(&mut cl[co as usize..(co + nc) as usize], |r| r.ts);
            }
            for &(pk, eo, ne, co, nc) in groups.iter() {
                let (i, j) = (eo as usize, (eo + ne) as usize);
                let pc = &cl[co as usize..(co + nc) as usize];
                starts.clear();
                ad.clear();
                sdi.clear();
                name.clear();
                dec_into(pk, &tab, &mut name);
                let mut k = i;
                let mut cp = 0usize;
                let mut runi = 0usize;
                let mut cur: usize = usize::MAX;
                let (mut ds, mut de, mut dr, mut dp) = (0i64, 0i64, 0i64, false);
                macro_rules! flush {
                    () => {
                        if cur != usize::MAX {
                            ur.uid.extend_from_slice(&name);
                            ur.uoff.push(ur.uid.len() as u32);
                            ur.day.push((day0 + cur as i64) as i32);
                            ur.sessions.push(ds);
                            ur.events.push(de);
                            ur.revenue.push(dr);
                            if dp {
                                let mut bi = 0;
                                for c in 1..ncat {
                                    if catrev[c] > catrev[bi] {
                                        bi = c;
                                    }
                                }
                                ur.top.push(bi as u8);
                                catrev.iter_mut().for_each(|x| *x = 0);
                            } else {
                                ur.top.push(255);
                            }
                        }
                    };
                }
                while k < j {
                    let s = ev[k].0;
                    let mut m = k + 1;
                    while m < j && ev[m].0 - ev[m - 1].0 <= GAP_MS {
                        m += 1;
                    }
                    let day = ((base + s as i64).div_euclid(DAY_MS) - day0) as usize;
                    if day != cur {
                        flush!();
                        ad.push((day as u32, starts.len() as u32));
                        cur = day;
                        ds = 0;
                        de = 0;
                        dr = 0;
                        dp = false;
                    }
                    let mut srev = 0i64;
                    let mut spur = false;
                    for &(_, e) in &ev[k..m] {
                        if e >> 31 != 0 {
                            let pr = (e & 0xFF_FFFF) as u64;
                            srev += pr as i64;
                            spur = true;
                            catrev[((e >> 24) & 0x7F) as usize] += pr;
                        }
                    }
                    ds += 1;
                    de += (m - k) as i64;
                    dr += srev;
                    dp |= spur;
                    while cp < pc.len() && pc[cp].ts <= s {
                        if cp == 0 || pc[cp].ts != pc[runi].ts || (pc[cp].cid != pc[runi].cid && kless(pc[cp].cid, pc[runi].cid, &tab)) {
                            runi = cp;
                        }
                        cp += 1;
                    }
                    let c = if cp > 0 && s - pc[runi].ts <= WEEK_MS { pc[runi].camp } else { organic };
                    let ci = c as usize * ndays + day;
                    camp[ci][0] += 1;
                    if spur {
                        camp[ci][1] += 1;
                        crev[ci] += srev as u64;
                    }
                    starts.push(s);
                    sdi.push(ad.len() as u32 - 1);
                    k = m;
                }
                flush!();
                ad.push((0, starts.len() as u32));
                if !pc.is_empty() {
                    let emit = |c: u32, a: u32, b: u32, camp: &mut Vec<u32>| {
                        let x0 = starts.partition_point(|&s| s < a);
                        let x1 = starts.partition_point(|&s| s <= b);
                        if x0 >= x1 {
                            return;
                        }
                        let (x0, x1) = (x0 as u32, x1 as u32);
                        for k in sdi[x0 as usize]..=sdi[x1 as usize - 1] {
                            let (d, b0) = ad[k as usize];
                            let n = x1.min(ad[k as usize + 1].1) - x0.max(b0);
                            camp[c as usize * ndays + d as usize] += n;
                        }
                    };
                    touched.clear();
                    for c in pc {
                        let ci = c.camp as usize;
                        if !open[ci] {
                            open[ci] = true;
                            touched.push(c.camp);
                            lo[ci] = c.ts;
                            hi[ci] = c.ts + WEEK_MS;
                        } else if c.ts <= hi[ci] {
                            hi[ci] = c.ts + WEEK_MS;
                        } else {
                            emit(c.camp, lo[ci], hi[ci], &mut casst);
                            lo[ci] = c.ts;
                            hi[ci] = c.ts + WEEK_MS;
                        }
                    }
                    for &c in &touched {
                        emit(c, lo[c as usize], hi[c as usize], &mut casst);
                        open[c as usize] = false;
                    }
                }
            }
        }
        (ur, camp, crev, casst)
    });
    lap("sessions");

    let mut tot = vec![[0u64; 4]; ncamp * ndays];
    for r in &results {
        for (i, a) in tot.iter_mut().enumerate() {
            a[0] += r.1[i][0] as u64;
            a[1] += r.1[i][1] as u64;
            a[2] += r.2[i];
            a[3] += r.3[i] as u64;
        }
    }
    let urs: Vec<&UserRows> = results.iter().map(|r| &r.0).collect();
    out::write_all(&outdir, &urs, &catnames, &tot, &cname, &cchan, ndays, day0, organic as usize);
    lap("write");
    std::process::exit(0);
}
