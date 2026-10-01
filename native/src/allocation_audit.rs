//! Independent test allocator observations. Fixed storage avoids allocations in
//! allocator callbacks. Production code never uses this as its budget authority.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
const SLOTS: usize = 65536;
#[derive(Clone, Copy)]
struct Slot {
    pointer: usize,
    bytes: usize,
    sequence: u64,
    domain: usize,
    category: usize,
    live: bool,
    measured: bool,
}
impl Slot {
    const EMPTY: Self = Self {
        pointer: 0,
        bytes: 0,
        sequence: 0,
        domain: 0,
        category: 0,
        live: false,
        measured: false,
    };
}
struct State {
    slots: [Slot; SLOTS],
    target: usize,
    next: u64,
    bound: u64,
    bound_bytes: u64,
    measured_bound: u64,
    measured_bytes: u64,
    unmatched: u64,
    live: [u64; 9],
    errors: u64,
}
static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<State> = Mutex::new(State {
    slots: [Slot::EMPTY; SLOTS],
    target: 0,
    next: 0,
    bound: 0,
    bound_bytes: 0,
    measured_bound: 0,
    measured_bytes: 0,
    unmatched: 0,
    live: [0; 9],
    errors: 0,
});
impl State {
    fn slot(&self, pointer: usize) -> Option<usize> {
        let start = (pointer >> 4).wrapping_mul(0x9e3779b1) % SLOTS;
        (0..SLOTS)
            .map(|n| (start + n) % SLOTS)
            .find(|i| self.slots[*i].pointer == 0 || self.slots[*i].pointer == pointer)
    }
}
fn allocated(state: &mut State, pointer: *mut u8, bytes: usize) {
    if pointer.is_null() {
        return;
    }
    let Some(i) = state.slot(pointer as usize) else {
        state.errors += 1;
        return;
    };
    if state.slots[i].live {
        state.errors += 1;
    }
    state.next += 1;
    state.unmatched += 1;
    state.slots[i] = Slot {
        pointer: pointer as usize,
        bytes,
        sequence: state.next,
        domain: 0,
        category: 0,
        live: true,
        measured: super::COUNTS.try_with(|counts| counts.get().is_some()).unwrap_or(false),
    };
}
fn freed(state: &mut State, pointer: *mut u8, bytes: usize) {
    if pointer.is_null() {
        return;
    }
    let Some(i) = state.slot(pointer as usize) else {
        return;
    };
    let slot = state.slots[i];
    if slot.pointer != pointer as usize || !slot.live {
        return;
    }
    if slot.bytes != bytes {
        state.errors += 1;
    }
    if slot.domain == state.target && slot.domain != 0 {
        state.live[slot.category] -= slot.bytes as u64;
    }
    state.slots[i].live = false;
}
// Serialize observed allocator calls themselves, so a freed address cannot be
// reused by another thread before its identity has been retired.
pub(super) unsafe fn alloc(layout: Layout, zero: bool) -> *mut u8 {
    let mut state = ACTIVE
        .load(Ordering::Acquire)
        .then(|| STATE.lock().unwrap());
    let p = if zero {
        System.alloc_zeroed(layout)
    } else {
        System.alloc(layout)
    };
    if let Some(state) = state.as_mut() {
        allocated(state, p, layout.size());
    }
    p
}
pub(super) unsafe fn dealloc(p: *mut u8, layout: Layout) {
    let mut state = ACTIVE
        .load(Ordering::Acquire)
        .then(|| STATE.lock().unwrap());
    System.dealloc(p, layout);
    if let Some(state) = state.as_mut() {
        freed(state, p, layout.size());
    }
}
pub(super) unsafe fn realloc(p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
    let mut state = ACTIVE
        .load(Ordering::Acquire)
        .then(|| STATE.lock().unwrap());
    let result = System.realloc(p, layout, size);
    if !result.is_null() {
        if let Some(state) = state.as_mut() {
            freed(state, p, layout.size());
            allocated(state, result, size);
        }
    }
    result
}
pub(super) fn claimed(domain: usize, pointer: usize, bytes: usize, category: usize, event: usize) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let mut state = STATE.lock().unwrap();
    if domain != state.target {
        return;
    }
    let Some(i) = state.slot(pointer) else {
        state.errors += 1;
        return;
    };
    if event == 1 {
        let slot = state.slots[i];
        if slot.pointer != pointer
            || !slot.live
            || slot.domain != domain
            || slot.bytes != bytes
            || category >= 9
        {
            state.errors += 1;
            return;
        }
        state.live[slot.category] -= bytes as u64;
        state.live[category] += bytes as u64;
        state.slots[i].category = category;
        return;
    }
    let slot = &mut state.slots[i];
    if !slot.live
        || slot.pointer != pointer
        || slot.bytes != bytes
        || slot.domain != 0
        || slot.sequence == 0
        || category >= 9
    {
        state.errors += 1;
        return;
    }
    slot.domain = domain;
    slot.category = category;
    if slot.measured {
        state.measured_bound += 1;
        state.measured_bytes += bytes as u64;
    }
    state.live[category] += bytes as u64;
    state.bound += 1;
    state.bound_bytes += bytes as u64;
    state.unmatched -= 1;
}
// A root's address does not exist when its allocation begins. Only the explicit
// bootstrap observer may select the domain of a session started with target 0.
// The physical allocation was already recorded independently by alloc().
pub(super) fn bootstrap_claimed(domain: usize, pointer: usize, bytes: usize, category: usize, event: usize) {
    {
        let mut state = STATE.lock().unwrap();
        assert!(ACTIVE.load(Ordering::Acquire));
        assert_eq!(state.target, 0);
        assert_ne!(domain, 0);
        assert_eq!(event, 0);
        state.target = domain;
    }
    claimed(domain, pointer, bytes, category, event);
}
#[derive(Debug, Clone, Copy)]
pub(super) struct Snapshot {
    pub measured_bound: u64,
    pub measured_bytes: u64,
    pub bound: u64,
    pub bound_bytes: u64,
    pub unmatched: u64,
    pub live: [u64; 9],
    pub errors: u64,
}
pub(super) fn snapshot() -> Snapshot {
    let s = STATE.lock().unwrap();
    Snapshot {
        measured_bound: s.measured_bound,
        measured_bytes: s.measured_bytes,
        bound: s.bound,
        bound_bytes: s.bound_bytes,
        unmatched: s.unmatched,
        live: s.live,
        errors: s.errors,
    }
}
pub(super) struct Session;
pub(super) fn start(domain: usize) -> Session {
    assert!(!ACTIVE.load(Ordering::Acquire));
    let mut s = STATE.lock().unwrap();
    for slot in &mut s.slots {
        *slot = Slot::EMPTY;
    }
    s.target = domain;
    s.next = 0;
    s.bound = 0;
    s.bound_bytes = 0;
    s.measured_bound = 0;
    s.measured_bytes = 0;
    s.unmatched = 0;
    s.live = [0; 9];
    s.errors = 0;
    ACTIVE.store(true, Ordering::Release);
    Session
}
impl Drop for Session {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}
