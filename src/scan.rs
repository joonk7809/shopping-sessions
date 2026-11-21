#![allow(unsafe_op_in_unsafe_fn)]
use std::arch::x86_64::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAXF: usize = 16;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Counts {
    pub good: u64,
    pub bad: u64,
}

pub fn reference<const N: usize, F: FnMut(&[&[u8]; N])>(buf: &[u8], mut f: F) -> Counts {
    let mut c = Counts::default();
    let body = buf.strip_suffix(b"\n").unwrap_or(buf);
    if buf.is_empty() {
        return c;
    }
    for line in body.split(|&b| b == b'\n') {
        let parts: Vec<&[u8]> = line.split(|&b| b == b',').collect();
        let ok = parts.len() == N
            && parts.iter().all(|p| {
                !p.contains(&b'"')
                    || (p.len() >= 2 && p[0] == b'"' && p[p.len() - 1] == b'"' && !p[1..p.len() - 1].contains(&b'"'))
            });
        if ok {
            let mut fs: [&[u8]; N] = [&[]; N];
            for (d, p) in fs.iter_mut().zip(&parts) {
                *d = if p.first() == Some(&b'"') { &p[1..p.len() - 1] } else { p };
            }
            f(&fs);
            c.good += 1;
        } else {
            c.bad += 1;
        }
    }
    c
}

macro_rules! clmul_prefix {
    ($q:expr) => {
        _mm_cvtsi128_si64(_mm_clmulepi64_si128(_mm_set_epi64x(0, $q as i64), _mm_set1_epi8(-1), 0)) as u64
    };
}

macro_rules! kernel {
    ($name:ident, $feat:literal, $masks:ident) => {
        #[target_feature(enable = $feat)]
        pub unsafe fn $name<const N: usize, F: FnMut(&[&[u8]; N])>(buf: &[u8], f: &mut F) -> Counts {
            let len = buf.len();
            let bp = buf.as_ptr();
            let mut st = [0usize; MAXF];
            let mut en = [0usize; MAXF];
            let mut k = 0usize;
            let mut fstart = 0usize;
            let mut c_s: u64 = 1;
            let mut c_c: u64 = 0;
            let mut c_p: u64 = 0;
            let mut lastp: u64 = 0;
            let mut rowbad: u64 = 0;
            let mut cnt = Counts::default();
            let mut tail = [0u8; 64];
            let mut i = 0usize;
            let need_virtual = len > 0 && *bp.add(len - 1) != b'\n';
            let pf = PREFETCH.load(Ordering::Relaxed);
            loop {
                let ptr = if i + 64 <= len {
                    bp.add(i)
                } else if i < len || (need_virtual && i == len) {
                    let r = len - i;
                    std::ptr::copy_nonoverlapping(bp.add(i), tail.as_mut_ptr(), r);
                    if need_virtual {
                        tail[r] = b'\n';
                    }
                    tail.as_ptr()
                } else {
                    break;
                };
                if pf != 0 {
                    _mm_prefetch::<_MM_HINT_T0>(bp.wrapping_add(i + pf) as *const i8);
                }
                let (cm, nl, q) = $masks!(ptr);
                let s = cm | nl;
                let starts = (s << 1) | c_s;
                c_s = s >> 63;
                let cq = q & !starts;
                let mut err = ((cq << 1) | c_c) & !s;
                c_c = cq >> 63;
                let p = clmul_prefix!(q) ^ 0u64.wrapping_sub(c_p);
                c_p = p >> 63;
                let mut bits = s;
                while bits != 0 {
                    let b = bits.trailing_zeros();
                    let pos = i + b as usize;
                    let idx = k.min(N);
                    st[idx] = fstart;
                    en[idx] = pos;
                    k += 1;
                    fstart = pos + 1;
                    let pb = (p >> b) & 1;
                    rowbad |= pb ^ lastp;
                    lastp = pb;
                    if (nl >> b) & 1 != 0 {
                        let m = u64::MAX >> (63 - b);
                        rowbad |= ((err & m) != 0) as u64;
                        err &= !m;
                        if k == N && rowbad == 0 {
                            let mut fs: [&[u8]; N] = [&[]; N];
                            for j in 0..N {
                                let a = st[j];
                                let e = en[j];
                                let qd = (a < e && *bp.add(a) == b'"') as usize;
                                fs[j] = std::slice::from_raw_parts(bp.add(a + qd), e - a - 2 * qd);
                            }
                            f(&fs);
                            cnt.good += 1;
                        } else {
                            cnt.bad += 1;
                        }
                        k = 0;
                        rowbad = 0;
                    }
                    bits &= bits - 1;
                }
                rowbad |= (err != 0) as u64;
                i += 64;
                if ptr == tail.as_ptr() {
                    break;
                }
            }
            cnt
        }
    };
}

macro_rules! masks512 {
    ($p:expr) => {{
        let v = _mm512_loadu_si512($p as *const _);
        (
            _mm512_cmpeq_epi8_mask(v, _mm512_set1_epi8(b',' as i8)),
            _mm512_cmpeq_epi8_mask(v, _mm512_set1_epi8(b'\n' as i8)),
            _mm512_cmpeq_epi8_mask(v, _mm512_set1_epi8(b'"' as i8)),
        )
    }};
}

macro_rules! masks256 {
    ($p:expr) => {{
        let a = _mm256_loadu_si256($p as *const __m256i);
        let b = _mm256_loadu_si256(($p as *const u8).add(32) as *const __m256i);
        let mm = |x: __m256i, y: __m256i, c: u8| {
            let cc = _mm256_set1_epi8(c as i8);
            (_mm256_movemask_epi8(_mm256_cmpeq_epi8(x, cc)) as u32 as u64)
                | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(y, cc)) as u32 as u64) << 32)
        };
        (mm(a, b, b','), mm(a, b, b'\n'), mm(a, b, b'"'))
    }};
}

kernel!(tok_avx512, "avx512f,avx512bw,pclmulqdq,bmi1,bmi2,popcnt", masks512);
kernel!(tok_avx2, "avx2,pclmulqdq,bmi1,bmi2,popcnt", masks256);

#[repr(align(64))]
struct Align64([u64; 16]);

pub static WINDOW: AtomicUsize = AtomicUsize::new(1 << 14);
pub static PREFETCH: AtomicUsize = AtomicUsize::new(4096);

#[target_feature(enable = "avx512f,avx512bw,avx512vbmi2,pclmulqdq,bmi1,bmi2,popcnt")]
pub unsafe fn tok_idx<const N: usize, const QF: bool, const VE: bool, F: FnMut(&[&[u8]; N])>(buf: &[u8], f: &mut F) -> Counts {
    let len = buf.len();
    assert!(len < 1 << 29);
    let bp = buf.as_ptr();
    let win = WINDOW.load(Ordering::Relaxed).max(64) & !63;
    let pf = PREFETCH.load(Ordering::Relaxed);
    let mut idx: Vec<u32> = vec![0; win + 256];
    let mut bad: Vec<u32> = vec![u32::MAX];
    let mut bi = 0usize;
    let mut cnt = Counts::default();
    let mut tail = [0u8; 64];
    let need_virtual = len > 0 && *bp.add(len - 1) != b'\n';
    let mut c_s: u64 = 1;
    let mut c_c: u64 = 0;
    let mut c_q: u64 = 0;
    let mut pm: u64 = 0;
    let mut lastp: u64 = 0;
    let mut pending = false;
    let mut iota = [0u8; 64];
    for (k, x) in iota.iter_mut().enumerate() {
        *x = (k * 4) as u8;
    }
    let iota4 = _mm512_loadu_si512(iota.as_ptr() as *const _);
    let one8 = _mm512_set1_epi8(1);
    let two8 = _mm512_set1_epi8(2);
    let one32 = _mm512_set1_epi32(1);
    let probe: &[u8] = &iota[3..5];
    let raw: [usize; 2] = std::mem::transmute(probe);
    let ve_ok = raw == [iota.as_ptr() as usize + 3, 2];
    let mut out = Align64([0u64; 16]);
    let bpv = _mm512_set1_epi64(bp as i64);
    let ix0 = _mm512_setr_epi64(0, 8, 1, 9, 2, 10, 3, 11);
    let ix1 = _mm512_setr_epi64(4, 12, 5, 13, 6, 14, 7, 15);
    let target: u32 = 1 << (N - 1);
    let lown: u32 = (1u32 << N) - 1;
    let mut n = 1usize;
    let mut i = 0usize;
    let mut done = false;
    while !done {
        let wend = i + win;
        if idx.len() < n + win + 256 {
            idx.resize(n + win + 256, 0);
        }
        let ip = idx.as_mut_ptr();
        while i < wend {
            let ptr = if i + 64 <= len {
                bp.add(i)
            } else if i < len || (need_virtual && i == len) {
                let r = len - i;
                std::ptr::copy_nonoverlapping(bp.add(i), tail.as_mut_ptr(), r);
                if need_virtual {
                    tail[r] = b'\n';
                }
                done = true;
                tail.as_ptr()
            } else {
                done = true;
                break;
            };
            if pf != 0 {
                _mm_prefetch::<_MM_HINT_T0>(bp.wrapping_add(i + pf) as *const i8);
            }
            let (cm, nl, q) = masks512!(ptr);
            let s = cm | nl;
            let starts = (s << 1) | c_s;
            c_s = s >> 63;
            let (errc, p, qf);
            if QF && q == 0 {
                errc = c_c & !s;
                c_c = 0;
                p = pm;
                qf = c_q & s;
                c_q = 0;
            } else {
                let cq = q & !starts;
                errc = ((cq << 1) | c_c) & !s;
                c_c = cq >> 63;
                p = clmul_prefix!(q) ^ pm;
                pm = 0u64.wrapping_sub(p >> 63);
                qf = ((q << 1) | c_q) & s;
                c_q = q >> 63;
            }
            if (errc | ((p ^ 0u64.wrapping_sub(lastp)) & s)) != 0 || pending {
                let mut bits = s;
                let mut e = errc;
                while bits != 0 {
                    let b = bits.trailing_zeros();
                    let below = e & ((1u64 << b) - 1);
                    let pb = (p >> b) & 1;
                    if pending || below != 0 || pb != lastp {
                        bad.insert(bad.len() - 1, ((i + b as usize + 1) as u32) << 2);
                    }
                    pending = false;
                    e &= !((1u64 << b) - 1);
                    lastp = pb;
                    bits &= bits - 1;
                }
                pending |= e != 0;
            }
            let v = _mm512_mask_add_epi8(iota4, nl, iota4, one8);
            let v = _mm512_mask_add_epi8(v, qf, v, two8);
            let c = _mm512_maskz_compress_epi8(s, v);
            let base = _mm512_set1_epi32(((i + 1) << 2) as i32);
            let k = s.count_ones() as usize;
            _mm512_storeu_si512(ip.add(n) as *mut _, _mm512_add_epi32(_mm512_cvtepu8_epi32(_mm512_castsi512_si128(c)), base));
            if k > 16 {
                for h in 1..4 {
                    let x = match h {
                        1 => _mm512_extracti32x4_epi32::<1>(c),
                        2 => _mm512_extracti32x4_epi32::<2>(c),
                        _ => _mm512_extracti32x4_epi32::<3>(c),
                    };
                    _mm512_storeu_si512(ip.add(n + 16 * h) as *mut _, _mm512_add_epi32(_mm512_cvtepu8_epi32(x), base));
                }
            }
            n += k;
            i += 64;
        }
        let mut last = n;
        while last > 1 && *ip.add(last - 1) & 1 == 0 {
            last -= 1;
        }
        let mut j = 1usize;
        while j < last {
            let v = _mm512_loadu_si512(ip.add(j) as *const _);
            let m = _mm512_test_epi32_mask(v, one32) as u32;
            if m & lown == target {
                let el = *ip.add(j + N - 1);
                if *bad.get_unchecked(bi) <= el {
                    while *bad.get_unchecked(bi) <= el {
                        bi += 1;
                    }
                    cnt.bad += 1;
                } else if VE && N <= 8 && ve_ok {
                    let w = _mm512_loadu_si512(ip.add(j - 1) as *const _);
                    let e = _mm512_alignr_epi32::<1>(w, w);
                    let q = _mm512_and_si512(_mm512_srli_epi32::<1>(e), one32);
                    let st = _mm512_add_epi32(_mm512_srli_epi32::<2>(w), q);
                    let ln = _mm512_sub_epi32(_mm512_sub_epi32(_mm512_sub_epi32(_mm512_srli_epi32::<2>(e), one32), q), st);
                    let p64 = _mm512_add_epi64(_mm512_cvtepu32_epi64(_mm512_castsi512_si256(st)), bpv);
                    let l64 = _mm512_cvtepu32_epi64(_mm512_castsi512_si256(ln));
                    let op = out.0.as_mut_ptr();
                    _mm512_store_si512(op as *mut _, _mm512_permutex2var_epi64(p64, ix0, l64));
                    _mm512_store_si512(op.add(8) as *mut _, _mm512_permutex2var_epi64(p64, ix1, l64));
                    f(&*(op as *const [&[u8]; N]));
                    cnt.good += 1;
                } else {
                    let mut fs: [&[u8]; N] = [&[]; N];
                    let mut prev = (*ip.add(j - 1) >> 2) as usize;
                    for t in 0..N {
                        let e = *ip.add(j + t);
                        let qd = ((e >> 1) & 1) as usize;
                        let end = (e >> 2) as usize - 1;
                        fs[t] = std::slice::from_raw_parts(bp.add(prev + qd), end - prev - 2 * qd);
                        prev = end + 1;
                    }
                    f(&fs);
                    cnt.good += 1;
                }
                j += N;
            } else {
                let mut jj = j;
                let mut mm = m;
                while mm == 0 {
                    jj += 16;
                    mm = _mm512_test_epi32_mask(_mm512_loadu_si512(ip.add(jj) as *const _), one32) as u32;
                }
                let nlj = jj + mm.trailing_zeros() as usize;
                let el = *ip.add(nlj);
                while *bad.get_unchecked(bi) <= el {
                    bi += 1;
                }
                cnt.bad += 1;
                j = nlj + 1;
            }
        }
        std::ptr::copy(ip.add(j - 1), ip, n - (j - 1));
        n -= j - 1;
        if bi > 0 && bi + 1 == bad.len() {
            bad.clear();
            bad.push(u32::MAX);
            bi = 0;
        }
    }
    cnt
}

pub fn tokenize<const N: usize, F: FnMut(&[&[u8]; N])>(buf: &[u8], mut f: F) -> Counts {
    assert!(N >= 1 && N < MAXF);
    unsafe {
        if is_x86_feature_detected!("avx512bw") && is_x86_feature_detected!("avx512vbmi2") && is_x86_feature_detected!("pclmulqdq") && buf.len() < 1 << 29 {
            tok_idx::<N, false, false, F>(buf, &mut f)
        } else if is_x86_feature_detected!("avx512bw") && is_x86_feature_detected!("pclmulqdq") {
            tok_avx512::<N, F>(buf, &mut f)
        } else if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("pclmulqdq") {
            tok_avx2::<N, F>(buf, &mut f)
        } else {
            reference::<N, F>(buf, f)
        }
    }
}
