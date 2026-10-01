//! Private ABI DTOs; vendor accounting structures are converted field by field.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ResourceStatistics {
    current: u64,
    peak: u64,
    live: u64,
    reserved: u64,
}
impl From<mcap::storage::ResourceStatistics> for ResourceStatistics {
    fn from(value: mcap::storage::ResourceStatistics) -> Self {
        Self {
            current: value.current,
            peak: value.peak,
            live: value.live,
            reserved: value.reserved,
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FlowStatistics {
    input_copy: u64,
    compaction_copy: u64,
    delivery_copy: u64,
    other_copy: u64,
    encoded_input: u64,
    encoded_output: u64,
    decoded_input: u64,
    decoded_output: u64,
    decode_started: u64,
    decode_completed: u64,
    cache_hits: u64,
    cache_misses: u64,
    cache_evictions: u64,
    reclaimed_bytes: u64,
}
impl From<mcap::storage::FlowStatistics> for FlowStatistics {
    fn from(value: mcap::storage::FlowStatistics) -> Self {
        Self {
            input_copy: value.input_copy,
            compaction_copy: value.compaction_copy,
            delivery_copy: value.delivery_copy,
            other_copy: value.other_copy,
            encoded_input: value.encoded_input,
            encoded_output: value.encoded_output,
            decoded_input: value.decoded_input,
            decoded_output: value.decoded_output,
            decode_started: value.decode_started,
            decode_completed: value.decode_completed,
            cache_hits: value.cache_hits,
            cache_misses: value.cache_misses,
            cache_evictions: value.cache_evictions,
            reclaimed_bytes: value.reclaimed_bytes,
        }
    }
}
#[repr(C)]
pub struct DetailedStatistics {
    resources: [ResourceStatistics; 9],
    allocation_count: u64,
    allocated_bytes: u64,
    rejected: u64,
    flow: FlowStatistics,
    lease_payload_bytes: u64,
    cache_payload_bytes: u64,
    current_bytes: u64,
    peak_bytes: u64,
    idle_bytes: u64,
    reallocation_count: u64,
    immediately_reclaimable_bytes: u64,
    mapped_logical_bytes: u64,
}
impl From<mcap::storage::DetailedStatistics> for DetailedStatistics {
    fn from(value: mcap::storage::DetailedStatistics) -> Self {
        Self {
            resources: value.resources.map(Into::into),
            flow: value.flow.into(),
            allocation_count: value.allocation_count,
            allocated_bytes: value.allocated_bytes,
            rejected: value.rejected,
            lease_payload_bytes: value.lease_payload_bytes,
            cache_payload_bytes: value.cache_payload_bytes,
            current_bytes: value.current_bytes,
            peak_bytes: value.peak_bytes,
            idle_bytes: value.idle_bytes,
            reallocation_count: value.reallocation_count,
            immediately_reclaimable_bytes: value.immediately_reclaimable_bytes,
            mapped_logical_bytes: value.mapped_logical_bytes,
        }
    }
}
const _: [(); 32] = [(); std::mem::size_of::<ResourceStatistics>()];
const _: [(); 112] = [(); std::mem::size_of::<FlowStatistics>()];
const _: [(); 488] = [(); std::mem::size_of::<DetailedStatistics>()];
const _: [(); 312] = [(); std::mem::offset_of!(DetailedStatistics, flow)];
const _: [(); 424] = [(); std::mem::offset_of!(DetailedStatistics, lease_payload_bytes)];
const _: [(); 480] = [(); std::mem::offset_of!(DetailedStatistics, mapped_logical_bytes)];
