use std::arch::x86_64::*;

#[repr(C, align(16))]
struct A16([[u8; 16]; 17]);

static RALIGN: A16 = {
    let mut t = [[0x80u8; 16]; 17];
    let mut n = 0;
    while n <= 16 {
        let mut i = 16 - n;
        while i < 16 {
            t[n][i] = (i - (16 - n)) as u8;
            i += 1;
        }
        n += 1;
    }
    A16(t)
};

static DIM: [u8; 16] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31, 0, 0, 0];
static DOY: [u16; 16] = [0, 306, 337, 0, 31, 61, 92, 122, 153, 184, 214, 245, 275, 0, 0, 0];

#[inline(always)]
unsafe fn ld(p: *const u8) -> __m128i {
    _mm_loadu_si128(p as *const __m128i)
}

#[inline(always)]
unsafe fn le(d: __m128i, lim: __m128i) -> i32 {
    _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_min_epu8(d, lim), d))
}

#[inline(always)]
unsafe fn digits16(v: __m128i) -> u64 {
    let a = _mm_maddubs_epi16(v, _mm_setr_epi8(10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1));
    let b = _mm_madd_epi16(a, _mm_setr_epi16(100, 1, 100, 1, 100, 1, 100, 1));
    let c = _mm_madd_epi16(_mm_packus_epi32(b, b), _mm_setr_epi16(10000, 1, 10000, 1, 10000, 1, 10000, 1));
    let q = _mm_cvtsi128_si64(c) as u64;
    (q & 0xFFFF_FFFF) * 100_000_000 + (q >> 32)
}

#[inline(always)]
unsafe fn num16(p: *const u8, n: usize) -> (bool, u64) {
    let d = _mm_sub_epi8(ld(p), _mm_set1_epi8(b'0' as i8));
    let ok = (!le(d, _mm_set1_epi8(9)) & ((1u32 << n) - 1) as i32) == 0;
    let v = _mm_shuffle_epi8(d, _mm_load_si128(RALIGN.0[n].as_ptr() as *const __m128i));
    (ok, digits16(v))
}

#[inline(always)]
fn days(y: u32, mo: u32, d: u32) -> i64 {
    let yy = y + 400 - (mo <= 2) as u32;
    let base = yy * 365 + yy / 4 - yy / 100 + yy / 400;
    base as i64 + DOY[mo as usize & 15] as i64 + d as i64 - 1 - 719468 - 146097
}

#[inline(always)]
unsafe fn dt(p: *const u8, f2: bool) -> Option<i64> {
    let da = _mm_sub_epi8(ld(p), _mm_setr_epi8(48, 48, 48, 48, 45, 48, 48, 45, 48, 48, 84, 48, 48, 58, 48, 48));
    let la = _mm_setr_epi8(9, 9, 9, 9, 0, 9, 9, 0, 9, 9, 0, 9, 9, 0, 9, 9);
    let (tb, lb, sb) = if f2 {
        (
            _mm_setr_epi8(58, 48, 48, 0, 48, 48, 58, 48, 48, 0, 0, 0, 0, 0, 0, 0),
            _mm_setr_epi8(0, 9, 9, -1, 9, 9, 0, 9, 9, -1, -1, -1, -1, -1, -1, -1),
            _mm_setr_epi8(1, 2, 4, 5, 7, 8, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128),
        )
    } else {
        (
            _mm_setr_epi8(58, 48, 48, 90, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
            _mm_setr_epi8(0, 9, 9, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
            _mm_setr_epi8(1, 2, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128, -128),
        )
    };
    let db = _mm_sub_epi8(ld(p.add(16)), tb);
    if (le(da, la) & le(db, lb)) != 0xFFFF {
        return None;
    }
    let w = _mm_setr_epi8(10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1);
    let va = _mm_maddubs_epi16(_mm_shuffle_epi8(da, _mm_setr_epi8(0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, -128, -128, -128, -128)), w);
    let vb = _mm_maddubs_epi16(_mm_shuffle_epi8(db, sb), w);
    let v = _mm_unpacklo_epi64(_mm_srli_si128(va, 8), vb);
    let hi = _mm_setr_epi16(23, 59, 0, 0, 59, 23, 59, 0);
    let rng = _mm_movemask_epi8(_mm_cmpgt_epi16(v, hi));
    let x = _mm_cvtsi128_si64(va) as u64;
    let z = _mm_cvtsi128_si64(v) as u64;
    let s = _mm_extract_epi64(v, 1) as u64;
    let y = (x & 0xFFFF) as u32 * 100 + ((x >> 16) & 0xFFFF) as u32;
    let mo = ((x >> 32) & 0xFFFF) as u32;
    let d = (x >> 48) as u32;
    let h = (z & 0xFFFF) as i64;
    let mi = ((z >> 16) & 0xFFFF) as i64;
    let se = (s & 0xFFFF) as i64;
    let oh = ((s >> 16) & 0xFFFF) as i64;
    let om = ((s >> 32) & 0xFFFF) as i64;
    let leap = (y & 3 == 0) & ((y % 25 != 0) | (y & 15 == 0));
    let dim = DIM[mo as usize & 15] as u32 + (leap & (mo == 2)) as u32;
    if (rng != 0) | (mo.wrapping_sub(1) >= 12) | (d.wrapping_sub(1) >= dim) {
        return None;
    }
    let o = oh * 3600 + om * 60;
    let o = if *p.add(19) == b'-' { -o } else { o };
    Some((days(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - o) * 1000)
}

#[cold]
#[inline(never)]
fn f3_slow(s: &[u8]) -> Option<i64> {
    crate::parse_f3(s)
}

#[inline(always)]
unsafe fn f3(p: *const u8, n: usize) -> Option<i64> {
    if n.wrapping_sub(1) < 16 {
        let (ok, v) = num16(p, n);
        if ok { Some(v as i64) } else { None }
    } else if n == 0 {
        None
    } else {
        f3_slow(std::slice::from_raw_parts(p, n))
    }
}

#[inline(always)]
pub unsafe fn ts_p(p: *const u8, n: usize) -> Option<i64> {
    let c19 = *p.add(19);
    if n == 20 && c19 == b'Z' {
        return dt(p, false);
    }
    if n == 25 && ((c19 == b'+') | (c19 == b'-')) && *p.add(22) == b':' {
        return dt(p, true);
    }
    f3(p, n)
}

#[inline(always)]
pub unsafe fn enc_p(p: *const u8, n: usize) -> Option<u64> {
    let x = ld(p);
    let rg = |lo: u8, span: u8| {
        let t = _mm_sub_epi8(x, _mm_set1_epi8(lo as i8));
        _mm_cmpeq_epi8(_mm_min_epu8(t, _mm_set1_epi8(span as i8)), t)
    };
    let c = _mm_or_si128(
        _mm_or_si128(
            _mm_and_si128(rg(b'0', 9), _mm_sub_epi8(x, _mm_set1_epi8(47))),
            _mm_and_si128(rg(b'A', 25), _mm_sub_epi8(x, _mm_set1_epi8(54))),
        ),
        _mm_or_si128(
            _mm_and_si128(rg(b'a', 25), _mm_sub_epi8(x, _mm_set1_epi8(59))),
            _mm_and_si128(_mm_cmpeq_epi8(x, _mm_set1_epi8(b'_' as i8)), _mm_set1_epi8(37)),
        ),
    );
    let keep = _mm_cmpgt_epi8(_mm_set1_epi8(n as i8), _mm_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15));
    let c = _mm_and_si128(c, keep);
    let zero = _mm_movemask_epi8(_mm_cmpeq_epi8(c, _mm_setzero_si128())) & _mm_movemask_epi8(keep);
    if (zero != 0) | (n.wrapping_sub(1) >= 10) {
        return None;
    }
    let a = _mm_maddubs_epi16(c, _mm_setr_epi8(64, 1, 64, 1, 64, 1, 64, 1, 64, 1, 64, 1, 64, 1, 64, 1));
    let b = _mm_madd_epi16(a, _mm_setr_epi16(4096, 1, 4096, 1, 4096, 1, 4096, 1));
    let lo = _mm_cvtsi128_si64(b) as u64;
    let l2 = _mm_extract_epi32(b, 2) as u64;
    Some(((lo & 0xFFFFFF) << 36) | ((lo >> 32) << 12) | (l2 >> 12))
}

#[cold]
pub fn price_slow(p: &[u8]) -> Option<u32> {
    let mut v: u64 = 0;
    let mut sig = 0;
    let mut ok = !p.is_empty();
    for &b in p {
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            ok = false;
            break;
        }
        if sig > 0 || d != 0 {
            sig += 1;
            if sig <= 9 {
                v = v * 10 + d as u64;
            }
        }
    }
    ok = ok && sig <= 8 && v >= 1 && v <= 10_000_000;
    if ok { Some(v as u32) } else { None }
}

#[inline(always)]
pub unsafe fn price_p(p: *const u8, n: usize) -> Option<u32> {
    if n.wrapping_sub(1) < 16 {
        let (ok, v) = num16(p, n);
        if ok & (v.wrapping_sub(1) < 10_000_000) { Some(v as u32) } else { None }
    } else if n == 0 {
        None
    } else {
        price_slow(std::slice::from_raw_parts(p, n))
    }
}

#[inline(always)]
pub unsafe fn etype_p(p: *const u8, n: usize) -> u8 {
    let w = std::ptr::read_unaligned(p as *const u64);
    let w4 = w as u32;
    let n4 = n == 4;
    (n4 & (w4 == u32::from_le_bytes(*b"view"))) as u8
        | (((n4 & (w4 == u32::from_le_bytes(*b"cart"))) as u8) << 1)
        | (((n == 8) & (w == u64::from_le_bytes(*b"purchase"))) as u8 * 3)
}

pub const PAD: usize = 32;

