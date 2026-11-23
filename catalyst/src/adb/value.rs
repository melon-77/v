//! The ADB value encoding (§B6, §B7).
//!
//! An [`Adb`] owns one ADB block payload. Values are 32-bit tags: a 4-bit
//! type and a 28-bit value or payload offset. Every access is bounds-checked
//! against the payload length; a reference that points outside the payload
//! or a value of the wrong type is an error (apk's C accessors silently
//! return 0/NULL/empty instead, §B7, which the spec recommends against).
//!
//! Readers built on this follow a fixed schema of bounded depth, so shared
//! values and reference cycles cannot cause unbounded work.

use crate::error::{Error, Result};

pub const TYPE_MASK: u32 = 0xf000_0000;
pub const VALUE_MASK: u32 = 0x0fff_ffff;

pub const TYPE_SPECIAL: u32 = 0x0000_0000;
pub const TYPE_INT: u32 = 0x1000_0000;
pub const TYPE_INT_32: u32 = 0x2000_0000;
pub const TYPE_INT_64: u32 = 0x3000_0000;
pub const TYPE_BLOB_8: u32 = 0x8000_0000;
pub const TYPE_BLOB_16: u32 = 0x9000_0000;
pub const TYPE_BLOB_32: u32 = 0xa000_0000;
pub const TYPE_ARRAY: u32 = 0xd000_0000;
pub const TYPE_OBJECT: u32 = 0xe000_0000;

/// Largest slot count of an object or array (`ADBI_MAX`).
pub const ADBI_MAX: u32 = 0x0100_0000;

/// A raw value tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Val(pub u32);

impl Val {
    pub const NULL: Val = Val(0);

    pub fn is_null(self) -> bool {
        self.0 == 0
    }
    pub fn type_tag(self) -> u32 {
        self.0 & TYPE_MASK
    }
    pub fn value(self) -> u32 {
        self.0 & VALUE_MASK
    }
}

/// One ADB block payload: the 8-byte `adb_hdr` followed by the value heap.
/// All value offsets are relative to its first byte.
#[derive(Clone)]
pub struct Adb {
    buf: Vec<u8>,
}

impl std::fmt::Debug for Adb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Adb({} bytes)", self.buf.len())
    }
}

impl Adb {
    /// Takes an ADB block payload. Requires the 8-byte header and
    /// `adb_compat_ver == 0`.
    pub fn new(buf: Vec<u8>) -> Result<Adb> {
        if buf.len() < 8 {
            return Err(Error::Format("ADB payload shorter than its header".into()));
        }
        if buf[0] != 0 {
            return Err(Error::Format(format!("unsupported ADB compat version {}", buf[0])));
        }
        Ok(Adb { buf })
    }

    /// The whole payload, as hashed and signed.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The root value (`adb_hdr.root`).
    pub fn root(&self) -> Val {
        Val(u32::from_le_bytes(self.buf[4..8].try_into().unwrap()))
    }

    /// `len` bytes at offset `off`, or an error if out of bounds.
    pub fn slice(&self, off: usize, len: usize) -> Result<&[u8]> {
        let end = off
            .checked_add(len)
            .ok_or_else(|| Error::Schema("value offset overflow".into()))?;
        self.buf
            .get(off..end)
            .ok_or_else(|| Error::Schema(format!("value at {off:#x}+{len} is outside the ADB block")))
    }

    fn u32_at(&self, off: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.slice(off, 4)?.try_into().unwrap()))
    }

    /// Reads an integer. NULL reads as 0 (absent and 0 are equivalent for
    /// apk, §B7.3). Any non-integer type is an error.
    pub fn int(&self, v: Val) -> Result<u64> {
        let off = v.value() as usize;
        match v.type_tag() {
            TYPE_SPECIAL if v.is_null() => Ok(0),
            TYPE_INT => Ok(v.value() as u64),
            TYPE_INT_32 => Ok(self.u32_at(off)? as u64),
            TYPE_INT_64 => Ok(u64::from_le_bytes(self.slice(off, 8)?.try_into().unwrap())),
            t => Err(Error::Schema(format!("expected an integer, found value type {:#x}", t >> 28))),
        }
    }

    /// Reads a blob. NULL reads as `None`; a zero-length blob is
    /// `Some(&[])`. Any non-blob type is an error.
    pub fn blob(&self, v: Val) -> Result<Option<&[u8]>> {
        let off = v.value() as usize;
        let (plen, len) = match v.type_tag() {
            TYPE_SPECIAL if v.is_null() => return Ok(None),
            TYPE_BLOB_8 => (1, self.slice(off, 1)?[0] as usize),
            TYPE_BLOB_16 => (2, u16::from_le_bytes(self.slice(off, 2)?.try_into().unwrap()) as usize),
            TYPE_BLOB_32 => (4, self.u32_at(off)? as usize),
            t => return Err(Error::Schema(format!("expected a blob, found value type {:#x}", t >> 28))),
        };
        Ok(Some(self.slice(off + plen, len)?))
    }

    /// Reads an object or array (both tags are accepted for either role,
    /// §D1.2). NULL reads as an empty container.
    pub fn obj(&self, v: Val) -> Result<Obj<'_>> {
        match v.type_tag() {
            TYPE_SPECIAL if v.is_null() => return Ok(Obj { adb: self, slots: &[] }),
            TYPE_ARRAY | TYPE_OBJECT => {}
            t => return Err(Error::Schema(format!("expected an object, found value type {:#x}", t >> 28))),
        }
        let off = v.value() as usize;
        let num = self.u32_at(off)?;
        if num == 0 || num > ADBI_MAX {
            return Err(Error::Schema(format!("object at {off:#x} has invalid slot count {num}")));
        }
        let slots = self.slice(off, num as usize * 4)?;
        Ok(Obj { adb: self, slots })
    }

    /// The root object.
    pub fn root_obj(&self) -> Result<Obj<'_>> {
        self.obj(self.root())
    }
}

/// A slot vector: an object (fields 1..) or an array (items 1..).
#[derive(Clone, Copy)]
pub struct Obj<'a> {
    adb: &'a Adb,
    /// All slots including slot 0 (the count); empty for a NULL container.
    slots: &'a [u8],
}

impl<'a> Obj<'a> {
    pub fn adb(&self) -> &'a Adb {
        self.adb
    }

    /// Slot count including slot 0 (1 for an empty container).
    pub fn num_slots(&self) -> usize {
        (self.slots.len() / 4).max(1)
    }

    /// Field `i` (1-based slot number). Slots past the end read as NULL.
    pub fn get(&self, i: usize) -> Val {
        if i == 0 || i >= self.slots.len() / 4 {
            return Val::NULL;
        }
        Val(u32::from_le_bytes(self.slots[i * 4..i * 4 + 4].try_into().unwrap()))
    }

    /// Number of array items.
    pub fn len(&self) -> usize {
        self.num_slots() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Array items in stored order (slots 1..).
    pub fn items(&self) -> impl Iterator<Item = Val> + 'a {
        let o = *self;
        (1..self.num_slots()).map(move |i| o.get(i))
    }

    pub fn int(&self, i: usize) -> Result<u64> {
        self.adb.int(self.get(i))
    }

    pub fn blob(&self, i: usize) -> Result<Option<&'a [u8]>> {
        self.adb.blob(self.get(i))
    }

    pub fn obj(&self, i: usize) -> Result<Obj<'a>> {
        self.adb.obj(self.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adb(heap: &[u8], root: u32) -> Adb {
        let mut v = vec![0, 0, 0, 0];
        v.extend_from_slice(&root.to_le_bytes());
        v.extend_from_slice(heap);
        Adb::new(v).unwrap()
    }

    #[test]
    fn example_b7_10() {
        // payload of the §B7.10 example
        let a = adb(&[1, b'x', 0, 0, 2, 0, 0, 0, 8, 0, 0, 0x80], 0xe000_000c);
        let o = a.root_obj().unwrap();
        assert_eq!(o.num_slots(), 2);
        assert_eq!(o.blob(1).unwrap(), Some(&b"x"[..]));
        assert_eq!(o.blob(2).unwrap(), None);
        assert_eq!(o.int(7).unwrap(), 0);
    }

    #[test]
    fn ints_and_blobs() {
        // heap at payload offset 8
        let mut heap = Vec::new();
        heap.extend_from_slice(&0x1234_5678u32.to_le_bytes()); // 8: u32
        heap.extend_from_slice(&0x1_0000_0000u64.to_le_bytes()); // 12: u64
        heap.extend_from_slice(&3u16.to_le_bytes()); // 20: blob16 len 3
        heap.extend_from_slice(b"abc");
        heap.push(0); // 25
        heap.extend_from_slice(&[0, 0]); // 26..28
        heap.extend_from_slice(&2u32.to_le_bytes()); // 28: blob32 len 2
        heap.extend_from_slice(b"hi");
        let a = adb(&heap, 0);
        assert_eq!(a.int(Val(TYPE_INT | 5)).unwrap(), 5);
        assert_eq!(a.int(Val(TYPE_INT_32 | 8)).unwrap(), 0x1234_5678);
        assert_eq!(a.int(Val(TYPE_INT_64 | 12)).unwrap(), 0x1_0000_0000);
        assert_eq!(a.blob(Val(TYPE_BLOB_16 | 20)).unwrap(), Some(&b"abc"[..]));
        assert_eq!(a.blob(Val(TYPE_BLOB_32 | 28)).unwrap(), Some(&b"hi"[..]));
        assert_eq!(a.blob(Val(TYPE_BLOB_8 | 25)).unwrap(), Some(&b""[..]));
        // out of bounds
        assert!(a.int(Val(TYPE_INT_64 | 30)).is_err());
        assert!(a.blob(Val(TYPE_BLOB_32 | 0x0fff_ffff)).is_err());
        assert!(a.blob(Val(TYPE_BLOB_8 | 33)).is_err()); // length 'i' -> past the end
        // wrong types
        assert!(a.int(Val(TYPE_BLOB_8 | 8)).is_err());
        assert!(a.blob(Val(TYPE_INT | 8)).is_err());
        assert!(a.obj(Val(TYPE_INT | 8)).is_err());
        assert!(a.int(Val(1)).is_err()); // TRUE
        assert!(a.int(Val(0xf000_0001)).is_err()); // ERROR tag
    }

    #[test]
    fn objects() {
        let mut heap = Vec::new();
        heap.extend_from_slice(&3u32.to_le_bytes()); // 8: num 3
        heap.extend_from_slice(&(TYPE_INT | 7).to_le_bytes());
        heap.extend_from_slice(&0u32.to_le_bytes());
        heap.extend_from_slice(&0u32.to_le_bytes()); // 20: num 0 (invalid)
        heap.extend_from_slice(&1000u32.to_le_bytes()); // 24: num 1000 (out of bounds)
        let a = adb(&heap, TYPE_ARRAY | 8);
        let o = a.root_obj().unwrap();
        assert_eq!(o.len(), 2);
        assert_eq!(o.items().collect::<Vec<_>>(), vec![Val(TYPE_INT | 7), Val::NULL]);
        assert!(a.obj(Val(TYPE_OBJECT | 20)).is_err());
        assert!(a.obj(Val(TYPE_OBJECT | 24)).is_err());
        assert!(a.obj(Val(TYPE_OBJECT | 26)).is_err());
        assert_eq!(a.obj(Val::NULL).unwrap().len(), 0);
        assert!(Adb::new(vec![1, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(Adb::new(vec![0; 7]).is_err());
    }
}
