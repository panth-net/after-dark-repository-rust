use core::fmt;

/// Every failure mode of resource-fork parsing.
///
/// Resource forks are untrusted input, so every read is bounds-checked and every
/// failure names the offset it happened at — silent truncation is how a
/// mis-decoded module turns into a subtly wrong render instead of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The fork is shorter than the 16-byte header.
    TooShort { len: usize },
    /// A header offset/length pair runs past the end of the fork.
    HeaderOutOfBounds {
        what: &'static str,
        offset: u32,
        len: u32,
        fork_len: usize,
    },
    /// A structural read ran past the end of the fork.
    OutOfBounds {
        what: &'static str,
        offset: usize,
        need: usize,
        fork_len: usize,
    },
    /// A resource's declared payload length exceeds the data area.
    ResourceOutOfBounds {
        res_type: [u8; 4],
        id: i16,
        offset: usize,
        size: u32,
        fork_len: usize,
    },
    /// Two resources share a (type, id). The Resource Manager would resolve this
    /// by search order; we refuse rather than guess.
    DuplicateResource { res_type: [u8; 4], id: i16 },
    /// MacBinary header did not validate.
    NotMacBinary,
    /// No HFS volume header was found anywhere in a disk image.
    NotAnHfsVolume,
    /// An HFS volume was found, but a structure inside it did not parse.
    HfsMalformed { what: &'static str },
    /// A fork being *written* would exceed a field the format cannot widen:
    /// resource data offsets are 24 bits and both map list offsets are 16.
    /// Truncating either would produce a fork that parses and is wrong.
    TooLargeToWrite { what: &'static str, value: usize, limit: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len } => {
                write!(f, "resource fork too short: {len} bytes, need at least 16")
            }
            Self::HeaderOutOfBounds {
                what,
                offset,
                len,
                fork_len,
            } => write!(
                f,
                "header field {what} out of bounds: offset {offset} + len {len} > fork length {fork_len}"
            ),
            Self::OutOfBounds {
                what,
                offset,
                need,
                fork_len,
            } => write!(
                f,
                "{what} out of bounds: need {need} bytes at offset {offset}, fork length {fork_len}"
            ),
            Self::ResourceOutOfBounds {
                res_type,
                id,
                offset,
                size,
                fork_len,
            } => write!(
                f,
                "resource '{}' {id} payload out of bounds: {size} bytes at offset {offset}, fork length {fork_len}",
                crate::macroman::decode(res_type)
            ),
            Self::DuplicateResource { res_type, id } => write!(
                f,
                "duplicate resource '{}' {id}",
                crate::macroman::decode(res_type)
            ),
            Self::NotMacBinary => write!(f, "not a valid MacBinary file"),
            Self::NotAnHfsVolume => write!(
                f,
                "no Macintosh HFS disk in this file — it may be a .sit or .zip that needs \
                 unpacking first, or an image of a different kind of disk"
            ),
            Self::HfsMalformed { what } => {
                write!(f, "the disk image is damaged or incomplete ({what})")
            }
            Self::TooLargeToWrite { what, value, limit } => write!(
                f,
                "cannot write resource fork: {what} would be {value}, and the format's limit is {limit}"
            ),
        }
    }
}

impl core::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;
