use std::sync::Mutex;

const BLK: usize = 1 << 16;
const SLAB: usize = 1 << 26;

struct Pool {
    free: Vec<usize>,
    cur: usize,
    end: usize,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { free: Vec::new(), cur: 0, end: 0 });

fn get() -> usize {
    let mut p = POOL.lock().unwrap();
    if let Some(b) = p.free.pop() {
        return b;
    }
    if p.cur == p.end {
        p.cur = unsafe { crate::alloc::map(SLAB) } as usize;
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
    const CAP: usize = BLK / std::mem::size_of::<T>();

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

    #[inline(always)]
    pub fn at(&self, j: usize) -> T {
        unsafe { *self.bl[j / Self::CAP].add(j % Self::CAP) }
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
