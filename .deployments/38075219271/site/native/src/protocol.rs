// Private ABI values mirrored in src/Native.Protocol.cs. Keep numeric compatibility.
pub(crate) mod writer_operation {
    pub const SCHEMA: u32 = 1;
    pub const CHANNEL: u32 = 2;
    pub const METADATA: u32 = 4;
    pub const ATTACHMENT: u32 = 5;
    pub const FLUSH: u32 = 6;
    pub const COMPLETE: u32 = 7;
    pub const START_ATTACHMENT: u32 = 8;
    pub const ATTACHMENT_BYTES: u32 = 9;
    pub const FINISH_ATTACHMENT: u32 = 10;
    pub const PRIVATE_RECORD: u32 = 11;
    pub const SUMMARY: u32 = 12;
    pub const FLUSH_TO_DISK: u32 = 13;
}
pub(crate) mod snapshot_operation {
    pub const SEEK_MESSAGE: u32 = 2;
    pub const METADATA: u32 = 3;
    pub const ATTACHMENT: u32 = 4;
    pub const MESSAGE_INDEXES: u32 = 5;
    pub const FOOTER: u32 = 6;
    pub const COMPRESSED_DATA_OFFSET: u32 = 8;
}
pub(crate) mod reader_kind {
    pub const SESSION: u32 = 0;
    pub const BUFFER: u32 = 1;
}
pub(crate) mod summary_source {
    pub const SNAPSHOT: u32 = 0;
    pub const ENGINE: u32 = 1;
    pub const WRITER: u32 = 2;
}
pub(crate) mod engine_kind {
    pub const LINEAR: u32 = 0;
    pub const SUMMARY: u32 = 1;
    pub const INDEXED: u32 = 2;
}
pub(crate) mod declaration_kind {
    pub const SCHEMA: u32 = 1;
    pub const CHANNEL: u32 = 2;
}
pub(crate) mod indexed_control {
    pub const INSERT_CHUNK: u32 = 0;
    pub const SET_RECORD_LENGTH_LIMIT: u32 = 1;
    pub const CLEAR_RECORD_LENGTH_LIMIT: u32 = 2;
}
pub(crate) mod status {
    pub const SUCCESS: i32 = 0;
    pub const END: i32 = 1;
    pub const BUFFER_TOO_SMALL: i32 = 2;
    pub const ERROR: i32 = -1;
}
pub(crate) mod callback_status {
    pub const ACCEPTED: i32 = 0;
    pub const STOP: i32 = 1;
    // Produced by managed callbacks; native consumers reject every nonzero error.
    #[allow(dead_code)]
    pub const ERROR: i32 = -1;
}
pub(crate) mod writer_status {
    pub const SAFE_REJECTION: i32 = -2;
}
pub(crate) mod reader_open_status {
    pub const BUFFERED_SORT_REQUIRED: i32 = 3;
}
pub(crate) mod batch_status {
    pub const VISITOR_STOPPED: i32 = 3;
}
pub(crate) mod engine_event {
    pub const END: u32 = 0;
    pub const READ: u32 = 1;
    pub const SEEK: u32 = 2;
    pub const RECORD: u32 = 3;
    pub const MESSAGE: u32 = 4;
    pub const READ_CHUNK: u32 = 5;
}
pub(crate) mod buffer_mode {
    pub const LINEAR: u32 = 0;
    pub const SANS_MAGIC: u32 = 1;
    pub const FLATTEN_CHUNKS: u32 = 2;
    pub const CHUNK: u32 = 3;
    pub const RAW_MESSAGES: u32 = 4;
    pub const MESSAGES: u32 = 5;
}
