//! Preallocated capacity tickets. Registration and release share the ledger lock.
use super::{MemoryBudget, OwnerKind, ResourceCategory, StorageLimit, StorageFailure, StorageFailureKind};
use crate::charged::ChargedBox;
use std::{cell::UnsafeCell, ptr::NonNull};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityWaitStatus { Idle, Waiting, Ready, Unavailable }

struct Links {
    previous: Option<NonNull<Node>>,
    next: Option<NonNull<Node>>,
    requested: usize,
    status: CapacityWaitStatus,
    external_only: bool,
}
struct Node(UnsafeCell<Links>);
// Every access to Links, including unlinking before deallocation, holds the
// owning domain's ledger mutex. Nodes never move after publication.
unsafe impl Send for Node {}
unsafe impl Sync for Node {}

#[derive(Default)]
pub(super) struct Waiters { head: Option<NonNull<Node>> }
// The list is only accessed under the domain mutex. Each registered ticket
// keeps both its node and domain alive until it unlinks under that same mutex.
unsafe impl Send for Waiters {}
impl Waiters {
    unsafe fn unlink(&mut self, pointer: NonNull<Node>) {
        let node = &mut *pointer.as_ref().0.get();
        if node.status == CapacityWaitStatus::Idle { return; }
        if let Some(previous) = node.previous {
            (*previous.as_ref().0.get()).next = node.next;
        } else { self.head = node.next; }
        if let Some(next) = node.next {
            (*next.as_ref().0.get()).previous = node.previous;
        }
        node.previous = None;
        node.next = None;
        node.status = CapacityWaitStatus::Idle;
    }
    unsafe fn insert(&mut self, pointer: NonNull<Node>, requested: usize, available: usize, external_only: bool) -> CapacityWaitStatus {
        self.unlink(pointer);
        let node = &mut *pointer.as_ref().0.get();
        node.requested = requested;
        node.external_only = external_only;
        node.status = if available >= requested { CapacityWaitStatus::Ready } else { CapacityWaitStatus::Waiting };
        node.next = self.head;
        if let Some(head) = self.head { (*head.as_ref().0.get()).previous = Some(pointer); }
        self.head = Some(pointer);
        node.status
    }
    pub(super) fn release(&mut self, available: usize, external: usize, active_releases: usize) {
        let mut next = self.head;
        while let Some(pointer) = next {
            // Readiness is latched: another operation may consume the capacity
            // before the waiter is scheduled. That waiter must still retry.
            let node = unsafe { &mut *pointer.as_ref().0.get() };
            if node.status != CapacityWaitStatus::Ready && available >= node.requested {
                node.status = CapacityWaitStatus::Ready;
            } else if node.status == CapacityWaitStatus::Waiting && node.external_only
                && active_releases == 0 && available.saturating_add(external) < node.requested {
                node.status = CapacityWaitStatus::Unavailable;
            }
            next = node.next;
        }
    }
}

/// Allocate before starting the operation that might need to wait. `arm` must
/// run after its failed attempt has rolled back, with the complete additional
/// capacity needed to retry (including any rolled-back operation storage).
/// The caller separately establishes that external owners can release enough
/// storage; a ticket is a notification mechanism, not a liveness guarantee.
pub struct CapacityWaitTicket {
    node: ChargedBox<Node>,
    domain: crate::storage::BudgetRef,
}
impl CapacityWaitTicket {
    pub fn new(domain: &crate::storage::BudgetRef) -> Result<Self, StorageFailure> {
        let node = ChargedBox::new_fixed(Node(UnsafeCell::new(Links {
            previous: None, next: None, requested: 0, status: CapacityWaitStatus::Idle, external_only: false,
        })), domain, ResourceCategory::Scratch)?;
        node.charge_owner(OwnerKind::Operation, true);
        Ok(Self { node, domain: domain.clone() })
    }
    /// Allocation-free atomic capacity check and registration.
    pub fn arm(&mut self, requested: usize) -> Result<CapacityWaitStatus, StorageFailure> {
        self.arm_inner(requested, false)
    }
    /// Register only if unique external ownership or an ending release can help.
    pub fn arm_for_external_release(&mut self, requested: usize) -> Result<CapacityWaitStatus, StorageFailure> {
        self.arm_inner(requested, true)
    }
    fn arm_inner(&mut self, requested: usize, external_only: bool) -> Result<CapacityWaitStatus, StorageFailure> {
        let mut state = self.domain.state.lock().unwrap();
        let pointer = NonNull::from(&*self.node);
        if requested > self.domain.limits.total {
            unsafe { state.waiters.unlink(pointer); }
            return Err(StorageFailure {
                details: StorageLimit {resource: "NativeDomain", limit: self.domain.limits.total, domain_limit: self.domain.limits.total,
                    requested, current: state.stats.current as usize, phase: "wait-registration"},
                kind: StorageFailureKind::PermanentLimit, terminal: false,
            });
        }
        if external_only {
            if let Err(error)=self.domain.retry_status_locked(&state,requested) {
                unsafe { state.waiters.unlink(pointer); }
                return Err(error);
            }
        }
        let available = state.available_capacity(self.domain.limits.total);
        Ok(unsafe { state.waiters.insert(pointer, requested, available, external_only) })
    }
    pub fn status(&self) -> CapacityWaitStatus {
        let _state = self.domain.state.lock().unwrap();
        unsafe { (*self.node.0.get()).status }
    }
    pub fn checked_status(&self) -> Result<CapacityWaitStatus, StorageFailure> {
        let state = self.domain.state.lock().unwrap();
        let node = unsafe { &*self.node.0.get() };
        if node.status == CapacityWaitStatus::Unavailable {
            return Err(self.domain.retry_error(&state, node.requested));
        }
        Ok(node.status)
    }
    pub fn cancel(&mut self) {
        let mut state = self.domain.state.lock().unwrap();
        unsafe { state.waiters.unlink(NonNull::from(&*self.node)); }
    }
}
impl Drop for CapacityWaitTicket {
    fn drop(&mut self) { self.cancel(); }
}

/// Marks a finite native lease destruction operation. It avoids treating the
/// interval between removing owner pins and physical deallocation as permanent
/// occupancy. No consumer callback or wait occurs inside this scope.
pub struct CapacityReleaseGuard { domain: crate::storage::BudgetRef }
impl Drop for CapacityReleaseGuard {
    fn drop(&mut self) {
        let mut state = self.domain.state.lock().unwrap();
        state.active_releases -= 1;
        self.domain.capacity_changed(&mut state);
    }
}
impl MemoryBudget {

    fn retry_error(&self, state: &super::State, requested: usize) -> StorageFailure {
        StorageFailure {details: StorageLimit {resource:"NativeDomain",limit:self.limits.total,domain_limit:self.limits.total,
            requested,current:state.stats.current as usize,phase:"wait-admission"},
            kind: StorageFailureKind::PermanentLimit, terminal: false}
    }
    fn retry_status_locked(&self, state: &super::State, requested: usize) -> Result<CapacityWaitStatus, StorageFailure> {
        if requested > self.limits.total { return Err(self.retry_error(state,requested)); }
        let available = state.available_capacity(self.limits.total);
        if available >= requested { return Ok(CapacityWaitStatus::Ready); }
        if state.active_releases != 0 || available.saturating_add(state.ownership.externally_releasable as usize) >= requested {
            Ok(CapacityWaitStatus::Waiting)
        } else { Err(self.retry_error(state,requested)) }
    }
    /// Admission snapshot for synchronous callers; this never blocks.
    pub fn retry_status(&self, requested: usize) -> Result<CapacityWaitStatus, StorageFailure> {
        self.retry_status_locked(&self.state.lock().unwrap(), requested)
    }
}


impl crate::storage::BudgetRef {
    pub fn begin_capacity_release(&self) -> CapacityReleaseGuard {
        let mut state = self.state.lock().unwrap();
        state.active_releases = state.active_releases.checked_add(1).expect("Release count overflow");
        CapacityReleaseGuard { domain: self.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::BudgetLimits;
    fn domain() -> crate::storage::BudgetRef {
        crate::storage::BudgetRef::new(BudgetLimits {total: (65536) + crate::storage::BudgetRef::allocation_size(), block: 65536, retained: 0}).unwrap()
    }
    #[test]
    fn checked_admission_uses_unique_releasable_capacity_not_any_lease() {
        let domain=domain();
        let mut ticket=CapacityWaitTicket::new(&domain).unwrap();
        let small=domain.reserve(128).unwrap();
        small.owner_reference(OwnerKind::Lease,true);
        small.owner_reference(OwnerKind::Lease,true);
        let fixed=domain.reserve(65536-domain.workload_statistics().current as usize).unwrap();
        assert_eq!(domain.ownership_statistics().externally_releasable,128);
        assert!(ticket.arm_for_external_release(4096).is_err());
        assert_eq!(ticket.status(),CapacityWaitStatus::Idle);
        assert_eq!(ticket.arm_for_external_release(128).unwrap(),CapacityWaitStatus::Waiting);
        // A parser pin makes even that lease unable to release the allocation.
        small.owner_reference(OwnerKind::Parser,true);
        assert_eq!(ticket.status(),CapacityWaitStatus::Unavailable);
        assert!(ticket.checked_status().is_err());
        assert!(ticket.arm_for_external_release(128).is_err());
        small.owner_reference(OwnerKind::Parser,false);
        drop((small,fixed));
        // Release between a failed allocation and admission needs no live lease.
        assert_eq!(ticket.arm_for_external_release(4096).unwrap(),CapacityWaitStatus::Ready);
        drop(ticket);
        assert_eq!(domain.workload_statistics().current,0);
        assert_eq!(domain.ownership_statistics(),Default::default());
    }
    #[test]
    fn ending_release_defers_admission_until_physical_storage_is_returned() {
        let domain=domain();
        let mut ticket=CapacityWaitTicket::new(&domain).unwrap();
        let returning=domain.reserve(4096).unwrap();
        returning.owner_reference(OwnerKind::Lease,true);
        let fixed=domain.reserve(65536-domain.workload_statistics().current as usize).unwrap();
        let release=domain.begin_capacity_release();
        returning.owner_reference(OwnerKind::Lease,false);
        assert_eq!(domain.ownership_statistics().externally_releasable,0);
        assert_eq!(ticket.arm_for_external_release(4096).unwrap(),CapacityWaitStatus::Waiting);
        drop(returning);
        drop(release);
        assert_eq!(ticket.checked_status().unwrap(),CapacityWaitStatus::Ready);
        let reoccupied=domain.reserve(4096).unwrap();
        let release=domain.begin_capacity_release();
        assert_eq!(ticket.arm_for_external_release(4096).unwrap(),CapacityWaitStatus::Waiting);
        drop(release);
        assert_eq!(ticket.status(),CapacityWaitStatus::Unavailable);
        assert!(ticket.checked_status().is_err());
        ticket.cancel();
        drop((reoccupied,fixed,ticket));
        assert_eq!(domain.workload_statistics().current,0);
    }
    #[test]
    fn release_registration_reoccupation_and_cancellation_are_not_lost() {
        let domain = domain();
        let mut ticket = CapacityWaitTicket::new(&domain).unwrap();
        let base = domain.workload_statistics().current as usize;
        let occupied = domain.reserve(65536-base).unwrap();
        assert_eq!(ticket.arm(4096).unwrap(), CapacityWaitStatus::Waiting);
        drop(occupied);
        let occupied = domain.reserve(65536-base).unwrap();
        assert_eq!(ticket.status(), CapacityWaitStatus::Ready);
        assert_eq!(ticket.arm(4096).unwrap(), CapacityWaitStatus::Waiting);
        ticket.cancel();
        drop(occupied);
        assert_eq!(ticket.status(), CapacityWaitStatus::Idle);
        // Release before registration is observed by the atomic capacity check.
        assert_eq!(ticket.arm(4096).unwrap(), CapacityWaitStatus::Ready);
        assert!(ticket.arm(domain.limits().total + 1).is_err());
        assert_eq!(ticket.status(), CapacityWaitStatus::Idle);
        drop(ticket);
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn post_rollback_registration_and_unrelated_domain_do_not_self_wake() {
        let domain = domain();
        let mut ticket = CapacityWaitTicket::new(&domain).unwrap();
        let base = domain.workload_statistics().current as usize;
        let occupied = domain.reserve(65536-base-512).unwrap();
        let temporary = domain.reserve(512).unwrap();
        assert!(domain.reserve(1024).is_err());
        drop(temporary);
        assert_eq!(ticket.arm(1536).unwrap(), CapacityWaitStatus::Waiting);
        let other = crate::storage::BudgetRef::new(Default::default()).unwrap();
        drop(other.reserve(16384).unwrap());
        // A partial release which still cannot satisfy this request is not a wake.
        drop(domain.reserve(256).unwrap());
        assert_eq!(ticket.status(), CapacityWaitStatus::Waiting);
        drop(occupied);
        assert_eq!(ticket.status(), CapacityWaitStatus::Ready);
    }
    #[test]
    fn tickets_with_different_demands_and_dropped_nodes_keep_list_valid() {
        let domain = domain();
        let mut first = CapacityWaitTicket::new(&domain).unwrap();
        let mut middle = CapacityWaitTicket::new(&domain).unwrap();
        let mut last = CapacityWaitTicket::new(&domain).unwrap();
        let base = domain.workload_statistics().current as usize;
        let mut occupied = domain.reserve(65536-base).unwrap();
        first.arm(4096).unwrap(); middle.arm(8192).unwrap(); last.arm(16384).unwrap();
        drop(middle);
        occupied.resize(occupied.bytes()-4096).unwrap();
        assert_eq!(first.status(), CapacityWaitStatus::Ready);
        assert_eq!(last.status(), CapacityWaitStatus::Waiting);
        first.cancel();
        drop(last);
        drop(occupied);
        drop(first);
        assert_eq!(domain.workload_statistics().current, 0);
    }
    #[test]
    fn concurrent_release_and_reoccupation_before_observation_latches_ready() {
        let domain = domain();
        let mut ticket = CapacityWaitTicket::new(&domain).unwrap();
        let size = 65536-domain.workload_statistics().current as usize;
        let occupied = domain.reserve(size).unwrap();
        ticket.arm(4096).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                drop(occupied);
                let replacement = domain.reserve(size).unwrap();
                barrier.wait(); barrier.wait();
                drop(replacement);
            });
            barrier.wait();
            assert_eq!(domain.workload_statistics().current, 65536);
            assert_eq!(ticket.status(), CapacityWaitStatus::Ready);
            barrier.wait();
        });
    }
    #[test]
    fn ticket_allocation_refusal_has_no_registration_or_charge() {
        let domain = domain();
        domain.fail_allocation_at(0);
        assert!(CapacityWaitTicket::new(&domain).is_err());
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
        assert!(domain.state.lock().unwrap().waiters.head.is_none());
    }
    #[test]
    fn returning_storage_to_the_idle_pool_makes_capacity_available() {
        let domain = crate::storage::BudgetRef::new(BudgetLimits {total:(65536) + crate::storage::BudgetRef::allocation_size(), block:65536, retained:65536}).unwrap();
        let mut ticket = CapacityWaitTicket::new(&domain).unwrap();
        let block = domain.allocate(4096, ResourceCategory::Input).unwrap();
        let occupied = domain.reserve(65536-domain.workload_statistics().current as usize).unwrap();
        ticket.arm(4096).unwrap();
        assert_eq!(ticket.status(), CapacityWaitStatus::Waiting);
        drop(block);
        assert_eq!(domain.workload_statistics().current, 65536);
        assert!(domain.workload_statistics().retained >= 4096);
        assert_eq!(ticket.status(), CapacityWaitStatus::Ready);
        // The next real allocation can reclaim the pool without exceeding the domain.
        let replacement = domain.reserve(4096).unwrap();
        drop((replacement, occupied, ticket));
        assert_eq!(domain.workload_statistics().current, 0);
    }
}
