use common::*;
use segment::*;

use std::cell::RefCell;
use std::mem::size_of;
use std::mem::transmute;
use std::ptr;
use std::ptr::copy;
use std::ptr::copy_nonoverlapping;
use std::sync::{Arc,Mutex};

//==----------------------------------------------------==//
//      Entry header
//==----------------------------------------------------==//

/// Describe entry in the log. Format is:
///     | EntryHeader | Key bytes | Data bytes |
/// This struct MUST NOT contain any pointers.
#[derive(Debug)]
#[repr(packed)]
pub struct EntryHeader {
    keylen: u32,
    datalen: u32,
}

// TODO can I get rid of most of this?
// e.g. use std::ptr::read / write instead?
impl EntryHeader {

    pub fn new(desc: &ObjDesc) -> Self {
        assert!(desc.keylen() <= usize::max_value());
        assert!(desc.getvalue() != None);
        EntryHeader {
            keylen: desc.keylen() as u32,
            datalen: desc.valuelen(),
        }
    }

    pub fn empty() -> Self {
        EntryHeader {
            keylen: 0 as u32,
            datalen: 0 as u32,
        }
    }

    pub fn getdatalen(&self) -> u32 { self.datalen }
    pub fn getkeylen(&self) -> u32 { self.keylen }
    pub fn object_length(&self) -> u32 { self.datalen + self.keylen }
    pub fn len_with_header(&self) -> usize {
        (self.object_length() as usize) + size_of::<EntryHeader>()
    }

    /// Size of this (entire) entry in the log.
    pub fn len(&self) -> usize {
        size_of::<EntryHeader>() +
            self.keylen as usize +
            self.datalen as usize
    }

    pub fn as_ptr(&self) -> *const u8 {
        let addr: *const u8;
        unsafe { addr = transmute(self); }
        addr
    }

    /// Give the starting address of the object in the log, provided
    /// the address of this EntryHeader within the log.
    pub fn data_address(&self, entry: usize) -> *const u8 {
        (entry + size_of::<EntryHeader>() + self.keylen as usize)
            as *mut u8
    }

    #[cfg(test)]
    pub fn set_key_len(&mut self, l: u32) { self.keylen = l; }

    #[cfg(test)]
    pub fn set_data_len(&mut self, l: u32) { self.datalen = l; }
}


//==----------------------------------------------------==//
//      Log head
//==----------------------------------------------------==//

pub type LogHeadRef = Arc<RefCell<LogHead>>;

pub struct LogHead {
    segment: Option<SegmentRef>,
    manager: SegmentManagerRef,
}

// TODO when head is rolled, don't want to contend with other threads
// when handing it off to the compactor. we could keep a 'closed
// segment pool' with each log head. then periodically merge them into
// the compactor. this pool could be a concurrent queue with atomic
// push/pop. for now we just shove it into the compactor directly.

impl LogHead {

    pub fn new(manager: SegmentManagerRef) -> Self {
        LogHead { segment: None, manager: manager }
    }

    pub fn append(&mut self, buf: &ObjDesc) -> Status {
        // allocate if head not exist
        if let None = self.segment {
            if let Err(code) = self.roll() {
                return Err(code);
            }
        }
        if !rbm!(self.segment).can_hold(buf) {
            if let Err(code) = self.roll() {
                return Err(code);
            }
        }
        let mut seg = rbm!(self.segment);
        let va: usize;
        match seg.append(buf) {
            Ok(va_) => va = va_,
            Err(_) => panic!("has space but append failed"),
        }
        Ok(va)
    }

    //
    // --- Private methods ---
    //

    /// Replace the head segment.
    fn replace(&mut self) -> Status {
        match self.manager.lock() {
            Ok(mut manager) => {
                self.segment = manager.alloc();
            },
            Err(_) => panic!("lock poison"),
        }
        match self.segment {
            None => Err(ErrorCode::OutOfMemory),
            _ => Ok(1),
        }
    }

    /// Upon closing a head segment, add reference to the recently
    /// closed list for the compaction code to pick up.
    /// TODO move to local head-specific pool to avoid locking
    fn add_closed(&mut self) {
        if let Some(segref) = self.segment.clone() {
            match self.manager.lock() {
                Ok(mut manager) => {
                    manager.add_closed(&segref);
                },
                Err(_) => panic!("lock poison"),
            }
        }
    }

    /// Roll head. Close current and allocate new.
    fn roll(&mut self) -> Status {
        match self.segment.clone() {
            None => self.replace(),
            Some(segref) => {
                segref.borrow_mut().close();
                self.add_closed();
                self.replace()
            },
        }
    }

}

//==----------------------------------------------------==//
//      The log
//==----------------------------------------------------==//

pub struct Log {
    head: LogHeadRef, // TODO make multiple
    manager: SegmentManagerRef,
    epochs: EpochTableRef,
}

impl Log {

    pub fn new(manager: SegmentManagerRef) -> Self {
        let epochs = match manager.lock() {
            Err(_) => panic!("lock poison"),
            Ok(guard) => guard.epochs(),
        };
        Log {
            head: Arc::new(RefCell::new(LogHead::new(manager.clone()))),
            manager: manager.clone(),
            epochs: epochs,
        }
    }

    /// Append an object to the log. If successful, returns the
    /// virtual address within the log inside Ok().
    /// FIXME check key is valid UTF-8
    pub fn append(&mut self, buf: &ObjDesc) -> Status {
        // 1. determine log head to use
        let head = &self.head;
        // 2. call append on the log head
        match head.borrow_mut().append(buf) {
            e @ Err(_) => return e,
            Ok(va) => {
                match self.manager.lock() {
                    Err(_) => panic!("lock poison"),
                    Ok(guard) => {
                        let idx = guard.segment_of(va);
                        assert_eq!(idx.is_some(), true);
                        let len = buf.len_with_header();
                        self.epochs.incr_live(idx.unwrap(), len);
                    },
                } // manager lock
                Ok(va)
            },
        } // head append
    }

    pub fn enable_cleaning(&mut self) {
        unimplemented!();
    }

    pub fn disable_cleaning(&mut self) {
        unimplemented!();
    }

    //
    // --- Internal methods used for testing only ---
    //

    #[cfg(test)]
    pub fn epochs(&self) -> EpochTableRef { self.epochs.clone() }
}

//==----------------------------------------------------==//
//      Unit tests
//==----------------------------------------------------==//

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::mem::size_of;
    use std::mem::transmute;
    use std::ptr;
    use std::rc::Rc;
    use std::sync::{Arc,Mutex};

    use test::Bencher;
    use segment::*;
    use common::*;

    use super::super::logger;

    #[test]
    fn log_alloc_until_full() {
        logger::enable();
        let memlen = 1<<23;
        let numseg = memlen / SEGMENT_SIZE;
        let manager = segmgr_ref!(0, SEGMENT_SIZE, memlen);
        let mut log = Log::new(manager);
        let key: &'static str = "keykeykeykey";
        let val: &'static str = "valuevaluevalue";
        let obj = ObjDesc::new(key, Some(val.as_ptr()), val.len() as u32);
        loop {
            match log.append(&obj) {
                Ok(ign) => {},
                Err(code) => match code {
                    ErrorCode::OutOfMemory => break,
                    _ => panic!("filling log returned {:?}", code),
                },
            }
        }
    }

    // TODO fill log 50%, delete random items, then manually force
    // cleaning to test it

    // FIXME rewrite these unit tests

    #[test]
    fn entry_header_readwrite() {
        logger::enable();
        // get some raw memory
        let mem: Box<[u8;32]> = Box::new([0 as u8; 32]);
        let ptr = Box::into_raw(mem);

        // put a header into it with known values
        let mut header = EntryHeader::empty();
        header.set_key_len(5);
        header.set_data_len(7);
        assert_eq!(header.getkeylen(), 5);
        assert_eq!(header.getdatalen(), 7);

        let len = size_of::<EntryHeader>();
        unsafe {
            ptr::write(ptr as *mut EntryHeader, header);
        }

        // reset our copy, and re-read from raw memory
        unsafe {
            header = ptr::read(ptr as *const EntryHeader);
        }
        assert_eq!(header.getkeylen(), 5);
        assert_eq!(header.getdatalen(), 7);

        // free the original memory again
        let mem = unsafe { Box::from_raw(ptr) };
    }
}
