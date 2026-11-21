use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr::{copy_nonoverlapping, null_mut};

extern "C" {
    fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8;
    fn munmap(a: *mut u8, l: usize) -> i32;
    fn mremap(a: *mut u8, o: usize, n: usize, f: i32, ...) -> *mut u8;
    fn madvise(a: *mut u8, l: usize, adv: i32) -> i32;
}

const BIG: usize = 1 << 20;
const HP: usize = 1 << 21;

fn big(l: &Layout) -> bool {
    l.size() >= BIG && l.align() <= 4096
}

fn span(n: usize) -> usize {
    (n + HP - 1) & !(HP - 1)
}

pub unsafe fn map(n: usize) -> *mut u8 {
    let r = mmap(null_mut(), n + HP, 3, 0x22, -1, 0);
    if r as isize == -1 {
        return null_mut();
    }
    let a = ((r as usize + HP - 1) & !(HP - 1)) as *mut u8;
    let head = a as usize - r as usize;
    if head > 0 {
        munmap(r, head);
    }
    let tail = HP - head;
    if tail > 0 {
        munmap(a.add(n), tail);
    }
    madvise(a, n, 14);
    a
}

pub struct Huge;

unsafe impl GlobalAlloc for Huge {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if big(&l) {
            map(span(l.size()))
        } else {
            System.alloc(l)
        }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if big(&l) {
            map(span(l.size()))
        } else {
            System.alloc_zeroed(l)
        }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if big(&l) {
            munmap(p, span(l.size()));
        } else {
            System.dealloc(p, l)
        }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let nl = Layout::from_size_align_unchecked(n, l.align());
        match (big(&l), big(&nl)) {
            (true, true) => {
                let (o, m) = (span(l.size()), span(n));
                if o == m {
                    return p;
                }
                let r = mremap(p, o, m, 1);
                if r as isize == -1 {
                    return null_mut();
                }
                madvise(r, m, 14);
                r
            }
            (false, false) => System.realloc(p, l, n),
            _ => {
                let r = self.alloc(nl);
                if !r.is_null() {
                    copy_nonoverlapping(p, r, l.size().min(n));
                    self.dealloc(p, l);
                }
                r
            }
        }
    }
}
