use std::arch::x86_64::*;
use std::sync::Mutex;

const BLK: usize = 1 << 14;
const SLAB: usize = 1 << 26;

struct Pool {
    free: Vec<usize>,
    ready: Vec<usize>,
    used: usize,
    cur: usize,
    end: usize,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { free: Vec::new(), ready: Vec::new(), used: 0, cur: 0, end: 0 });

extern "C" {
    fn sched_setscheduler(pid: i32, pol: i32, prm: *const i32) -> i32;
}

pub fn prefault<F: FnOnce() -> usize + Send + 'static>(need: F) {
    std::thread::spawn(move || {
        unsafe { sched_setscheduler(0, 5, &0) };
        let n = need().div_ceil(SLAB);
        loop {
            {
                let p = POOL.lock().unwrap();
                if p.used + p.ready.len() >= n {
                    break;
                }
            }
            let s = unsafe { crate::alloc::map(SLAB) };
            for o in (0..SLAB).step_by(1 << 21) {
                unsafe { s.add(o).write_volatile(0) };
            }
            POOL.lock().unwrap().ready.push(s as usize);
        }
    });
}

fn get() -> usize {
    let mut p = POOL.lock().unwrap();
    if let Some(b) = p.free.pop() {
        return b;
    }
    if p.cur == p.end {
        p.used += 1;
        p.cur = match p.ready.pop() {
            Some(r) => r,
            None => unsafe { crate::alloc::map(SLAB) as usize },
        };
        p.end = p.cur + SLAB;
    }
    p.cur += BLK;
    p.cur - BLK
}

pub struct Seq<T> {
    bl: Vec<*mut T>,
    cur: *mut T,
    end: *mut T,
    len: usize,
}

unsafe impl<T: Send> Send for Seq<T> {}
unsafe impl<T: Sync> Sync for Seq<T> {}

impl<T: Copy> Seq<T> {
    const CAP: usize = BLK / std::mem::size_of::<T>() / 16 * 16;

    pub fn new() -> Self {
        Seq { bl: Vec::new(), cur: std::ptr::null_mut(), end: std::ptr::null_mut(), len: 0 }
    }

    #[inline(always)]
    pub fn push(&mut self, x: T) {
        if self.cur == self.end {
            let b = get() as *mut T;
            self.bl.push(b);
            self.cur = b;
            self.end = unsafe { b.add(Self::CAP) };
        }
        unsafe {
            self.cur.write(x);
            self.cur = self.cur.add(1);
        }
        self.len += 1;
    }

    #[inline(always)]
    pub fn put16(&mut self, g: &[T; 16]) {
        if self.cur == self.end {
            let b = get() as *mut T;
            self.bl.push(b);
            self.cur = b;
            self.end = unsafe { b.add(Self::CAP) };
        }
        unsafe {
            let s = g.as_ptr() as *const __m256i;
            let d = self.cur as *mut __m256i;
            for i in 0..std::mem::size_of::<[T; 16]>() / 32 {
                _mm256_stream_si256(d.add(i), _mm256_load_si256(s.add(i)));
            }
            self.cur = self.cur.add(16);
        }
        self.len += 16;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn chunks(&self) -> impl Iterator<Item = &[T]> {
        let n = self.len;
        self.bl.iter().enumerate().map(move |(i, &b)| unsafe { std::slice::from_raw_parts(b, (n - i * Self::CAP).min(Self::CAP)) })
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks().flatten()
    }
}

impl<T> Drop for Seq<T> {
    fn drop(&mut self) {
        if !self.bl.is_empty() {
            let mut p = POOL.lock().unwrap();
            p.free.extend(self.bl.iter().map(|&b| b as usize));
        }
    }
}

#[repr(C, align(64))]
struct G<T>([T; 16]);

pub struct Wc<T> {
    g: Vec<G<T>>,
    n: Vec<u8>,
}

impl<T: Copy> Wc<T> {
    pub fn new(np: usize, z: T) -> Self {
        assert!(std::mem::size_of::<[T; 16]>() % 64 == 0);
        Wc { g: (0..np).map(|_| G([z; 16])).collect(), n: vec![0; np] }
    }

    #[inline(always)]
    pub fn push(&mut self, q: usize, r: T, s: &mut [Seq<T>]) {
        let k = self.n[q] as usize;
        self.g[q].0[k] = r;
        if k == 15 {
            s[q].put16(&self.g[q].0);
            self.n[q] = 0;
        } else {
            self.n[q] = k as u8 + 1;
        }
    }

    pub fn flush(&self, s: &mut [Seq<T>]) {
        for (q, x) in s.iter_mut().enumerate() {
            for k in 0..self.n[q] as usize {
                x.push(self.g[q].0[k]);
            }
        }
        unsafe { _mm_sfence() };
    }
}
