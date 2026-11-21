use std::fs::File;
use std::io::{self, IoSlice, Write};
use std::path::Path;

pub const NULL: u8 = u8::MAX;
const ROWS: usize = 1 << 16;

#[derive(Clone, Copy)]
pub struct Strs<'a> {
    pub off: &'a [u32],
    pub dat: &'a [u8],
}

#[derive(Clone, Copy)]
pub enum Ints<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    U32(&'a [u32]),
    I32(&'a [i32]),
    I64(&'a [i64]),
}

pub struct UserDaily<'a> {
    pub user_id: Strs<'a>,
    pub day_base: i32,
    pub day: Ints<'a>,
    pub sessions: Ints<'a>,
    pub events: Ints<'a>,
    pub revenue_cents: Ints<'a>,
    pub top_category: &'a [u8],
    pub categories: &'a [&'a [u8]],
}

pub struct CampaignDaily<'a> {
    pub campaign_id: Strs<'a>,
    pub channel: Strs<'a>,
    pub day_base: i32,
    pub day: Ints<'a>,
    pub sessions: Ints<'a>,
    pub purchasing_sessions: Ints<'a>,
    pub revenue_cents: Ints<'a>,
    pub assisted_sessions: Ints<'a>,
}

impl<'a> Ints<'a> {
    fn slice(self, a: usize, b: usize) -> Ints<'a> {
        match self {
            Ints::U8(x) => Ints::U8(&x[a..b]),
            Ints::U16(x) => Ints::U16(&x[a..b]),
            Ints::U32(x) => Ints::U32(&x[a..b]),
            Ints::I32(x) => Ints::I32(&x[a..b]),
            Ints::I64(x) => Ints::I64(&x[a..b]),
        }
    }
    fn spec(self) -> bool {
        matches!(self, Ints::U32(_) | Ints::I64(_))
    }
}

#[derive(Clone, Copy)]
enum Col<'a> {
    S(Strs<'a>),
    I(Ints<'a>, i64, bool),
    C(&'a [u8], &'a [&'a [u8]]),
}

trait V: Copy {
    fn v(self) -> i64;
}

macro_rules! v {
    ($($t:ty),*) => {$(impl V for $t {
        fn v(self) -> i64 {
            self as i64
        }
    })*};
}
v!(u8, u16, u32, i32, i64);

fn bytes<T>(s: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}

fn varint(b: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        b.push(v as u8 | 0x80);
        v >>= 7;
    }
    b.push(v as u8);
}

fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn width(max: u64) -> u32 {
    64 - max.leading_zeros()
}

#[inline(always)]
unsafe fn pack8(p: *mut u8, g: [u64; 8], w: u32) {
    unsafe {
        if w <= 8 {
            let mut a = 0u64;
            for i in 0..8 {
                a |= g[i] << (i as u32 * w);
            }
            (p as *mut u64).write_unaligned(a);
        } else if w <= 16 {
            let mut a = 0u128;
            for i in 0..8 {
                a |= (g[i] as u128) << (i as u32 * w);
            }
            (p as *mut u128).write_unaligned(a);
        } else if w <= 32 {
            let (mut l, mut h) = (0u128, 0u128);
            for i in 0..8 {
                let o = i as u32 * w;
                let v = g[i] as u128;
                if o < 128 {
                    l |= v << o;
                    if o + w > 128 {
                        h |= v >> (128 - o);
                    }
                } else {
                    h |= v << (o - 128);
                }
            }
            (p as *mut u128).write_unaligned(l);
            (p.add(16) as *mut u128).write_unaligned(h);
        } else {
            let (mut acc, mut bits, mut q) = (0u128, 0u32, p);
            for v in g {
                acc |= (v as u128) << bits;
                bits += w;
                while bits >= 8 {
                    *q = acc as u8;
                    q = q.add(1);
                    acc >>= 8;
                    bits -= 8;
                }
            }
        }
    }
}

fn hybrid<T: V>(b: &mut Vec<u8>, xs: &[T], lo: i64, w: u32) {
    varint(b, (xs.len().div_ceil(8) as u64) << 1 | 1);
    let start = b.len();
    let total = xs.len().div_ceil(8) * w as usize;
    b.reserve(total + 32);
    let ex = xs.chunks_exact(8);
    let rem = ex.remainder();
    unsafe {
        let mut p = b.as_mut_ptr().add(start);
        for c in ex {
            pack8(p, std::array::from_fn(|i| c[i].v().wrapping_sub(lo) as u64), w);
            p = p.add(w as usize);
        }
        if !rem.is_empty() {
            pack8(p, std::array::from_fn(|i| rem.get(i).map_or(0, |x| x.v().wrapping_sub(lo) as u64)), w);
        }
        b.set_len(start + total);
    }
}

fn dbp(b: &mut Vec<u8>, xs: &[i32]) {
    let n = xs.len();
    varint(b, 128);
    varint(b, 4);
    varint(b, n as u64);
    varint(b, zz(xs[0] as i64));
    let mut r = [0i64; 128];
    let mut i = 1;
    while i < n {
        let k = (n - i).min(128);
        if k == 128 {
            let c: &[i32; 129] = xs[i - 1..i + 128].try_into().unwrap();
            for j in 0..128 {
                r[j] = c[j + 1] as i64 - c[j] as i64;
            }
        } else {
            for j in 0..k {
                r[j] = xs[i + j] as i64 - xs[i + j - 1] as i64;
            }
        }
        let mn = r[..k].iter().copied().fold(i64::MAX, i64::min);
        r[k..].fill(mn);
        varint(b, zz(mn));
        let mut ws = [0u8; 4];
        for q in 0..4 {
            ws[q] = width(r[q * 32..q * 32 + 32].iter().fold(0, |a, &x| a | (x - mn) as u64)) as u8;
        }
        b.extend_from_slice(&ws);
        b.reserve(1024 + 32);
        unsafe {
            let mut p = b.as_mut_ptr().add(b.len());
            for q in 0..k.div_ceil(32) {
                let w = ws[q] as u32;
                for g in 0..4 {
                    let o = q * 32 + g * 8;
                    pack8(p, std::array::from_fn(|j| (r[o + j] - mn) as u64), w);
                    p = p.add(w as usize);
                }
            }
            b.set_len(p.offset_from(b.as_ptr()) as usize);
        }
        i += k;
    }
}

struct T {
    b: Vec<u8>,
    last: i16,
    stack: Vec<i16>,
}

impl T {
    fn fh(&mut self, id: i16, ty: u8) {
        let d = id - self.last;
        if d > 0 && d <= 15 {
            self.b.push((d as u8) << 4 | ty);
        } else {
            self.b.push(ty);
            varint(&mut self.b, zz(id as i64));
        }
        self.last = id;
    }
    fn i32(&mut self, id: i16, v: i32) {
        self.fh(id, 5);
        varint(&mut self.b, zz(v as i64));
    }
    fn i64(&mut self, id: i16, v: i64) {
        self.fh(id, 6);
        varint(&mut self.b, zz(v));
    }
    fn bin(&mut self, id: i16, v: &[u8]) {
        self.fh(id, 8);
        varint(&mut self.b, v.len() as u64);
        self.b.extend_from_slice(v);
    }
    fn begin(&mut self, id: i16) {
        self.fh(id, 12);
        self.push();
    }
    fn push(&mut self) {
        self.stack.push(self.last);
        self.last = 0;
    }
    fn end(&mut self) {
        self.b.push(0);
        self.last = self.stack.pop().unwrap();
    }
    fn list(&mut self, id: i16, ty: u8, n: usize) {
        self.fh(id, 9);
        if n < 15 {
            self.b.push((n as u8) << 4 | ty);
        } else {
            self.b.push(0xF0 | ty);
            varint(&mut self.b, n as u64);
        }
    }
}

struct Sink {
    f: File,
    pos: u64,
    t: T,
    buf: Vec<u8>,
}

struct Meta {
    enc: i32,
    dict: bool,
    start: u64,
    data: u64,
    size: u64,
}

impl Sink {
    fn put(&mut self, parts: &mut [IoSlice]) -> io::Result<()> {
        self.pos += parts.iter().map(|p| p.len() as u64).sum::<u64>();
        let mut s = parts;
        while !s.is_empty() {
            let n = self.f.write_vectored(s)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            IoSlice::advance_slices(&mut s, n);
        }
        Ok(())
    }

    fn page(&mut self, kind: i32, rows: usize, enc: i32, data: &[u8]) -> io::Result<()> {
        let buf = std::mem::take(&mut self.buf);
        let r = self.page2(kind, rows, enc, &buf, data);
        self.buf = buf;
        r
    }

    fn page2(&mut self, kind: i32, rows: usize, enc: i32, a: &[u8], b: &[u8]) -> io::Result<()> {
        let len = (a.len() + b.len()) as i32;
        let t = &mut self.t;
        t.b.clear();
        t.last = 0;
        t.i32(1, kind);
        t.i32(2, len);
        t.i32(3, len);
        t.begin(if kind == 2 { 7 } else { 5 });
        t.i32(1, rows as i32);
        t.i32(2, enc);
        if kind == 0 {
            t.i32(3, 3);
            t.i32(4, 3);
        }
        t.end();
        t.b.push(0);
        let h = std::mem::take(&mut t.b);
        let r = self.put(&mut [IoSlice::new(&h), IoSlice::new(a), IoSlice::new(b)]);
        self.t.b = h;
        r
    }

    fn ints<X: V>(&mut self, xs: &[X], k: i64, el: usize, m: &mut Meta) -> io::Result<bool> {
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for &x in xs {
            lo = lo.min(x.v());
            hi = hi.max(x.v());
        }
        if el == 4 && (lo + k < i32::MIN as i64 || hi + k > i32::MAX as i64) {
            return Ok(false);
        }
        let n = xs.len();
        let span = (hi - lo) as u64;
        let w = width(span).max(1);
        self.buf.clear();
        if span < 1 << 16 && (span as usize + 1) * el + n * w as usize / 8 < n * el / 2 {
            m.dict = true;
            for v in lo..=hi {
                self.buf.extend_from_slice(&(v + k).to_le_bytes()[..el]);
            }
            self.page(2, span as usize + 1, 0, &[])?;
            m.data = self.pos;
            m.enc = 8;
            self.buf.clear();
            self.buf.push(w as u8);
            hybrid(&mut self.buf, xs, lo, w);
            self.page(0, n, 8, &[])?;
        } else if std::mem::size_of::<X>() == el && k == 0 {
            self.page2(0, n, 0, &[], bytes(xs))?;
        } else {
            if el == 8 {
                self.buf.extend(xs.iter().flat_map(|x| (x.v() + k).to_le_bytes()));
            } else {
                self.buf.extend(xs.iter().flat_map(|x| ((x.v() + k) as i32).to_le_bytes()));
            }
            self.page(0, n, 0, &[])?;
        }
        Ok(true)
    }

    fn strs(&mut self, x: Strs, ls: &mut Vec<i32>, m: &mut Meta) -> io::Result<()> {
        let off = x.off;
        let n = off.len() - 1;
        m.enc = 6;
        self.buf.clear();
        let l0 = off[1] - off[0];
        if off.windows(2).all(|w| w[1] - w[0] == l0) {
            varint(&mut self.buf, 128);
            varint(&mut self.buf, 4);
            varint(&mut self.buf, n as u64);
            varint(&mut self.buf, zz(l0 as i64));
            self.buf.resize(self.buf.len() + (n - 1).div_ceil(128) * 5, 0);
        } else {
            ls.clear();
            ls.extend(off.windows(2).map(|w| (w[1] - w[0]) as i32));
            dbp(&mut self.buf, ls);
        }
        self.page(0, n, 6, &x.dat[off[0] as usize..off[n] as usize])
    }

    fn cats(&mut self, xs: &[u8], dict: &[&[u8]], nn: &mut Vec<u8>, m: &mut Meta) -> io::Result<()> {
        m.dict = true;
        self.buf.clear();
        for d in dict {
            self.buf.extend_from_slice(&(d.len() as u32).to_le_bytes());
            self.buf.extend_from_slice(d);
        }
        self.page(2, dict.len(), 0, &[])?;
        m.data = self.pos;
        m.enc = 8;
        let w = width(dict.len().saturating_sub(1) as u64).max(1);
        let n = xs.len();
        let b = &mut self.buf;
        b.clear();
        b.extend_from_slice(&[0; 4]);
        nn.clear();
        nn.reserve(n);
        let mut k = 0;
        unsafe {
            let p = nn.as_mut_ptr();
            for &x in xs {
                *p.add(k) = x;
                k += (x != NULL) as usize;
            }
            nn.set_len(k);
        }
        if k == n || k == 0 {
            varint(b, (n as u64) << 1);
            b.push((k != 0) as u8);
        } else {
            varint(b, (n.div_ceil(8) as u64) << 1 | 1);
            let ex = xs.chunks_exact(8);
            let rem = ex.remainder();
            for g in ex {
                let mut a = 0u8;
                for i in 0..8 {
                    a |= ((g[i] != NULL) as u8) << i;
                }
                b.push(a);
            }
            if !rem.is_empty() {
                b.push(rem.iter().enumerate().fold(0u8, |a, (i, &x)| a | ((x != NULL) as u8) << i));
            }
        }
        let dl = (b.len() - 4) as u32;
        b[..4].copy_from_slice(&dl.to_le_bytes());
        b.push(w as u8);
        if k != 0 {
            hybrid(b, nn, 0, w);
        }
        self.page(0, n, 8, &[])
    }
}

fn slice<'a>(c: Col<'a>, a: usize, b: usize) -> Col<'a> {
    match c {
        Col::S(x) => Col::S(Strs { off: &x.off[a..=b], dat: x.dat }),
        Col::I(x, k, d) => Col::I(x.slice(a, b), k, d),
        Col::C(x, d) => Col::C(&x[a..b], d),
    }
}

fn table(path: &Path, cols: &[(&str, Col)], n: usize, wide: bool) -> io::Result<bool> {
    let t = T { b: Vec::with_capacity(1 << 16), last: 0, stack: Vec::new() };
    let mut s = Sink { f: File::create(path)?, pos: 0, t, buf: Vec::new() };
    s.put(&mut [IoSlice::new(b"PAR1")])?;
    let (mut ls, mut nn) = (Vec::new(), Vec::new());
    let mut rgs: Vec<(usize, Vec<Meta>)> = Vec::new();
    let mut a = 0;
    while a < n {
        let b = (a + ROWS).min(n);
        let mut ms = Vec::with_capacity(cols.len());
        for &(_, c) in cols {
            let start = s.pos;
            let mut m = Meta { enc: 0, dict: false, start, data: start, size: 0 };
            match slice(c, a, b) {
                Col::S(x) => s.strs(x, &mut ls, &mut m)?,
                Col::C(x, d) => s.cats(x, d, &mut nn, &mut m)?,
                Col::I(x, k, _) => {
                    let el = if wide && x.spec() { 8 } else { 4 };
                    let ok = match x {
                        Ints::U8(x) => s.ints(x, k, el, &mut m)?,
                        Ints::U16(x) => s.ints(x, k, el, &mut m)?,
                        Ints::U32(x) => s.ints(x, k, el, &mut m)?,
                        Ints::I32(x) => s.ints(x, k, el, &mut m)?,
                        Ints::I64(x) => s.ints(x, k, el, &mut m)?,
                    };
                    if !ok {
                        return Ok(false);
                    }
                }
            }
            m.size = s.pos - start;
            ms.push(m);
        }
        rgs.push((b - a, ms));
        a = b;
    }
    let ty = |c: &Col| match c {
        Col::S(_) | Col::C(..) => 6,
        Col::I(x, ..) => 1 + (wide && x.spec()) as i32,
    };
    let t = &mut s.t;
    t.b.clear();
    t.last = 0;
    t.i32(1, 1);
    t.list(2, 12, cols.len() + 1);
    t.push();
    t.bin(4, b"schema");
    t.i32(5, cols.len() as i32);
    t.end();
    for (name, c) in cols {
        t.push();
        t.i32(1, ty(c));
        t.i32(3, matches!(c, Col::C(..)) as i32);
        t.bin(4, name.as_bytes());
        let logical = match c {
            Col::S(_) | Col::C(..) => 1,
            Col::I(_, _, d) => 6 * *d as i16,
        };
        if logical != 0 {
            t.i32(6, if logical == 1 { 0 } else { 6 });
            t.begin(10);
            t.begin(logical);
            t.end();
            t.end();
        }
        t.end();
    }
    t.i64(3, n as i64);
    t.list(4, 12, rgs.len());
    for (rows, ms) in &rgs {
        t.push();
        t.list(1, 12, ms.len());
        for (m, (name, c)) in ms.iter().zip(cols) {
            t.push();
            t.i64(2, m.start as i64);
            t.begin(3);
            t.i32(1, ty(c));
            let encs: &[i64] = if m.dict { &[0, 3, 8] } else { &[m.enc as i64, 3] };
            t.list(2, 5, encs.len());
            for &e in encs {
                varint(&mut t.b, zz(e));
            }
            t.list(3, 8, 1);
            varint(&mut t.b, name.len() as u64);
            t.b.extend_from_slice(name.as_bytes());
            t.i32(4, 0);
            t.i64(5, *rows as i64);
            t.i64(6, m.size as i64);
            t.i64(7, m.size as i64);
            t.i64(9, m.data as i64);
            if m.dict {
                t.i64(11, m.start as i64);
            }
            t.end();
            t.end();
        }
        t.i64(2, ms.iter().map(|m| m.size).sum::<u64>() as i64);
        t.i64(3, *rows as i64);
        t.end();
    }
    t.b.push(0);
    let len = (t.b.len() as u32).to_le_bytes();
    let foot = std::mem::take(&mut t.b);
    s.put(&mut [IoSlice::new(&foot), IoSlice::new(&len), IoSlice::new(b"PAR1")])?;
    Ok(true)
}

fn files(dir: &Path, tag: &str, cols: &[(&str, Col)], n: usize, k: usize) -> io::Result<()> {
    let k = k.clamp(1, n.max(1));
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..k)
            .map(|i| {
                let (a, b) = (n * i / k, n * (i + 1) / k);
                sc.spawn(move || {
                    let part: Vec<(&str, Col)> = cols.iter().map(|&(nm, c)| (nm, slice(c, a, b))).collect();
                    let path = dir.join(format!("{tag}{i}.parquet"));
                    if !table(&path, &part, b - a, false)? {
                        table(&path, &part, b - a, true)?;
                    }
                    Ok(())
                })
            })
            .collect();
        hs.into_iter().try_for_each(|h| h.join().unwrap())
    })
}

pub fn user_daily(dir: &Path, tag: &str, x: &UserDaily, n: usize, k: usize) -> io::Result<()> {
    let cols = [
        ("user_id", Col::S(x.user_id)),
        ("day", Col::I(x.day, x.day_base as i64, true)),
        ("sessions", Col::I(x.sessions, 0, false)),
        ("events", Col::I(x.events, 0, false)),
        ("revenue_cents", Col::I(x.revenue_cents, 0, false)),
        ("top_category", Col::C(x.top_category, x.categories)),
    ];
    files(dir, tag, &cols, n, k)
}

pub fn campaign_daily(dir: &Path, tag: &str, x: &CampaignDaily, n: usize, k: usize) -> io::Result<()> {
    let cols = [
        ("campaign_id", Col::S(x.campaign_id)),
        ("channel", Col::S(x.channel)),
        ("day", Col::I(x.day, x.day_base as i64, true)),
        ("sessions", Col::I(x.sessions, 0, false)),
        ("purchasing_sessions", Col::I(x.purchasing_sessions, 0, false)),
        ("revenue_cents", Col::I(x.revenue_cents, 0, false)),
        ("assisted_sessions", Col::I(x.assisted_sessions, 0, false)),
    ];
    files(dir, tag, &cols, n, k)
}
