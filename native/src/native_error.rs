//! Inline binding errors. Upstream error payloads retain their original ownership;
//! this wrapper never adds a box merely to propagate or classify a failure.
use super::{Text, memory};
use std::{error::Error as StdError, fmt};

#[derive(Debug)]
enum Kind {
    Mcap(mcap::McapError),
    Io(std::io::Error),
    Limit(memory::Limit),
    Storage(mcap::storage::StorageLimit),
    Parse(binrw::Error),
    Bootstrap(mcap::storage::BootstrapError),
    Json(serde_json::Error),
    Utf8(std::str::Utf8Error),
    Integer(std::num::TryFromIntError),
    Slice(std::array::TryFromSliceError),
    Reserve(std::collections::TryReserveError),
    Message(Text<256>),
    #[cfg(test)]
    Test(&'static (dyn StdError + Sync)),
}

#[derive(Debug)]
pub struct NativeError {
    kind: Kind,
    safe_rejection: bool,
    advanced: bool,
}
impl NativeError {
    fn new(kind: Kind) -> Self { Self { kind, safe_rejection: false, advanced: false } }
    pub fn message(value: fmt::Arguments<'_>) -> Self { Self::new(Kind::Message(Text::display(&value))) }
    // Only the audited writer pre-mutation rejection sites call this constructor.
    pub fn safe_rejection(error: mcap::McapError) -> Self {
        Self { kind: Kind::Mcap(error), safe_rejection: true, advanced: false }
    }
    pub fn is_safe_rejection(&self) -> bool { self.safe_rejection }
    pub fn after_advance(mut self) -> Self {
        self.advanced = true;
        self.safe_rejection = false;
        self
    }
    pub fn advanced(&self) -> bool { self.advanced }
    pub fn as_ref(&self) -> &(dyn StdError + 'static) {
        match &self.kind {
            Kind::Mcap(v) => v, Kind::Io(v) => v, Kind::Limit(v) => v,
            Kind::Storage(v) => v, Kind::Parse(v) => v, Kind::Bootstrap(v) => v, Kind::Json(v) => v, Kind::Utf8(v) => v,
            Kind::Integer(v) => v, Kind::Slice(v) => v, Kind::Reserve(v) => v, Kind::Message(v) => v,
            #[cfg(test)] Kind::Test(v) => *v,
        }
    }
    pub fn downcast_ref<E: StdError + 'static>(&self) -> Option<&E> { self.as_ref().downcast_ref() }
    #[cfg(test)]
    pub fn test(error: &'static (dyn StdError + Sync)) -> Self { Self::new(Kind::Test(error)) }
}
impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(self.as_ref(), f) }
}
impl StdError for NativeError {}
macro_rules! conversion {
    ($source:ty, $variant:ident) => {
        impl From<$source> for NativeError {
            fn from(value: $source) -> Self { Self::new(Kind::$variant(value)) }
        }
    };
}
conversion!(mcap::McapError, Mcap);
impl From<mcap::storage::StorageFailure> for NativeError {
    fn from(value: mcap::storage::StorageFailure) -> Self {
        Self::new(Kind::Mcap(mcap::McapError::Storage(value)))
    }
}
conversion!(std::io::Error, Io);
conversion!(memory::Limit, Limit);
conversion!(mcap::storage::StorageLimit, Storage);
conversion!(binrw::Error, Parse);
conversion!(mcap::storage::BootstrapError, Bootstrap);
conversion!(serde_json::Error, Json);
conversion!(std::str::Utf8Error, Utf8);
conversion!(std::num::TryFromIntError, Integer);
conversion!(std::array::TryFromSliceError, Slice);
conversion!(std::collections::TryReserveError, Reserve);
impl From<&str> for NativeError {
    fn from(value: &str) -> Self { Self::message(format_args!("{value}")) }
}
