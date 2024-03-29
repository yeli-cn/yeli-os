use crate::mem::{address::PhysicalAddress, allocator::FrameAllocator, PAGE_SIZE};

pub struct MemoryArea {
    start: PhysicalAddress,
    size:  usize,
}

pub struct BumpAllocator {
    areas:  &'static [MemoryArea],
    offset: usize,
}

impl BumpAllocator {
    pub fn new(areas: &'static [MemoryArea], offset: usize) -> Self {
        Self { areas, offset }
    }
}

impl FrameAllocator for BumpAllocator {
    fn allocate(&mut self) -> Option<PhysicalAddress> {
        let mut offset = self.offset;
        for area in self.areas.iter() {
            if offset < area.size {
                self.offset += PAGE_SIZE;
                return Some(area.start + offset);
            }
            offset -= area.size;
        }
        None
    }

    fn free(&mut self, _pa: PhysicalAddress) {
        unimplemented!()
    }
}
