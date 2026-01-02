use super::{BufPool, BufferSize, FixedBuf};
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

// Buddy System Constants
const ARENA_SIZE: usize = 16 * 1024 * 1024; // 16MB Total
const MIN_BLOCK_SIZE: usize = 4096; // 4KB

// Number of orders: 4KB, 8KB, 16KB, 32KB, 64KB, 128KB, 256KB, 512KB, 1MB, 2MB, 4MB, 8MB, 16MB
// Orders:
// 0: 4KB
// 1: 8KB
// ...
// 12: 16MB
const NUM_ORDERS: usize = 13;

// Re-implementing simplified Buddy with explicit free tracking
// We will use "buddy_is_free" check by storing "order" in a map for every block start?
// Actually, standard optimization: use a bit per block to track "split".
// Or just maintain the free lists and do expensive check for MVP? No, must be performant.
//
// Improved approach:
// `tags`: Vec<u8>. Size = number of 4KB blocks (4096).
// `tags[i]` stores the order of the block starting at `i * 4KB`.
// Additionally we need to know if it is FREE or ALLOCATED.
// High bit = 1 implies ALLOCATED.
// 0x80 | order -> Allocated block of size order.
// order -> Free block of size order.

struct BuddyInner {
    memory: Vec<u8>,
    // Store free blocks for quick access
    free_lists: [Vec<usize>; NUM_ORDERS],
    // Tags for every 4KB block.
    // Index = offset / 4096.
    tags: Vec<u8>,
}

#[derive(Clone)]
pub struct BuddyPool {
    inner: Rc<RefCell<BuddyInner>>,
}

impl std::fmt::Debug for BuddyPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuddyPool").finish_non_exhaustive()
    }
}

impl BuddyPool {
    pub fn new() -> Self {
        Self::new_inner()
    }

    fn new_inner() -> Self {
        let mut memory = Vec::with_capacity(ARENA_SIZE);
        memory.resize(ARENA_SIZE, 0);

        let mut free_lists: [Vec<usize>; NUM_ORDERS] = Default::default();
        // One big block of max order
        free_lists[NUM_ORDERS - 1].push(0);

        let leaf_count = ARENA_SIZE / MIN_BLOCK_SIZE;
        let mut tags = vec![0u8; leaf_count];

        // Initial state: Block at 0 is free and Order 12.
        // Note: Only the tag at the START of the block is authoritative.
        tags[0] = (NUM_ORDERS - 1) as u8;
        // Others are irrelevant until split, but effectively "part of block at 0"

        Self {
            inner: Rc::new(RefCell::new(BuddyInner {
                memory,
                free_lists,
                tags,
            })),
        }
    }

    fn get_order(&self, size: usize) -> usize {
        if size <= MIN_BLOCK_SIZE {
            return 0;
        }
        let mut order = 0;
        let mut s = MIN_BLOCK_SIZE;
        while s < size {
            s <<= 1;
            order += 1;
        }
        order
    }

    pub fn alloc(&self, size: BufferSize) -> Option<FixedBuf<BuddyPool>> {
        self.alloc_mem(size.size())
            .map(|(ptr, cap, global_index, context)| {
                FixedBuf::new(self.clone(), ptr, cap, global_index, context)
            })
    }
}

impl BufPool for BuddyPool {
    fn new() -> Self {
        BuddyPool::new_inner()
    }

    fn alloc_mem(&self, size: usize) -> Option<(NonNull<u8>, usize, u16, usize)> {
        // If request > total arena, fail immediately (or fallback, but Buddy implies Arena constraints)
        if size > ARENA_SIZE {
            return None;
        }

        let needed_order = self.get_order(size);
        if needed_order >= NUM_ORDERS {
            return None;
        }

        let mut inner = self.inner.borrow_mut();

        // Find smallest available order >= needed_order
        for order in needed_order..NUM_ORDERS {
            if !inner.free_lists[order].is_empty() {
                // Found a block!
                let offset = inner.free_lists[order].pop().unwrap();

                // Split down to needed_order
                let mut curr_order = order;
                while curr_order > needed_order {
                    curr_order -= 1;
                    let buddy_offset = offset + (MIN_BLOCK_SIZE << curr_order);

                    // Add the buddy (upper half) to the free list of the lower order
                    inner.free_lists[curr_order].push(buddy_offset);

                    // Mark the buddy tag
                    let buddy_idx = buddy_offset / MIN_BLOCK_SIZE;
                    inner.tags[buddy_idx] = curr_order as u8;
                }

                // Now we have a block of 'needed_order' at 'offset'.
                // Mark as allocated
                let idx = offset / MIN_BLOCK_SIZE;
                inner.tags[idx] = (needed_order as u8) | 0x80;

                let ptr = unsafe { inner.memory.as_mut_ptr().add(offset) };

                // The 'context' can store the allocated size (power of two) or order
                // We'll store (AllocatorType << 24) | order? No, just order is enough if we trust our type?
                // But Wait, `context` is usize.
                // We just store the order. Or size.
                // Let's store the size (capacity) to be safe and compatible.
                // Or simply `offset`. wait, dealloc needs ptr to free.
                // `dealloc_mem` gets ptr + cap.
                // we can re-derive order from cap.
                // context is mostly ignored for Buddy?
                // Let's store 0.

                let alloc_size = MIN_BLOCK_SIZE << needed_order;

                // Global index: assuming the entire arena is registered as index 0.
                // But IO_URING uses (buf_index, address, len).
                // If we register indices 0..N for slabs, how does Buddy work?
                // Option A: Register each block? No.
                // Option B: Register the whole Arena as one buffer (index 0).
                // Then reads use (index 0, addr=ptr, len=size).
                // Yes, this works for io_uring!
                // So global_index is always 0.

                return Some((
                    NonNull::new(ptr).unwrap(),
                    alloc_size,
                    0, // Always index 0 for the whole arena
                    0, // context unused
                ));
            }
        }

        None
    }

    unsafe fn dealloc_mem(&self, ptr: NonNull<u8>, cap: usize, _context: usize) {
        let mut inner = self.inner.borrow_mut();
        let base_ptr = inner.memory.as_mut_ptr();
        let offset = unsafe { ptr.as_ptr().offset_from(base_ptr) } as usize;

        let order = self.get_order(cap);
        let mut curr_offset = offset;
        let mut curr_order = order;

        // Mark as free
        let idx = curr_offset / MIN_BLOCK_SIZE;
        inner.tags[idx] = curr_order as u8; // Clear allocated bit

        // Merge loop
        while curr_order < NUM_ORDERS - 1 {
            let block_size = MIN_BLOCK_SIZE << curr_order;
            // Buddy address logic
            // buddy of block at offset X of size S is X ^ S
            let buddy_offset = curr_offset ^ block_size;

            // Check if buddy is valid
            if buddy_offset >= ARENA_SIZE {
                break;
            }

            // Check buddy status
            let buddy_idx = buddy_offset / MIN_BLOCK_SIZE;
            let buddy_tag = inner.tags[buddy_idx];

            // Buddy must be Free (high bit 0) AND same order
            if buddy_tag != curr_order as u8 {
                break; // Cannot merge
            }

            // Remove buddy from free list
            // This is linear scan in the vector, which is slow-ish but fine for standard usage?
            // To optimize, use Doubly Linked List for free lists. But for now Vec::retain or find+swap_remove.
            if let Some(pos) = inner.free_lists[curr_order]
                .iter()
                .position(|&x| x == buddy_offset)
            {
                inner.free_lists[curr_order].swap_remove(pos);
            } else {
                // Should not happen if tag logic is correct
                break;
            }

            // Prepare for next level
            curr_offset = std::cmp::min(curr_offset, buddy_offset);
            curr_order += 1;

            // Update tag for the merged block
            let new_idx = curr_offset / MIN_BLOCK_SIZE;
            inner.tags[new_idx] = curr_order as u8;
        }

        // Push the final free block
        inner.free_lists[curr_order].push(curr_offset);
    }

    #[cfg(target_os = "linux")]
    fn get_registration_buffers(&self) -> Vec<libc::iovec> {
        let inner = self.inner.borrow();
        vec![libc::iovec {
            iov_base: inner.memory.as_ptr() as *mut _,
            iov_len: ARENA_SIZE,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buddy_alloc_merge() {
        let pool = BuddyPool::new();

        // Alloc 1: 4KB
        let buf1 = pool.alloc(BufferSize::Size4K).unwrap();
        assert_eq!(buf1.capacity(), 4096);
        let ptr1 = buf1.as_slice().as_ptr() as usize;

        // Alloc 2: 4KB
        let buf2 = pool.alloc(BufferSize::Size4K).unwrap();
        let ptr2 = buf2.as_slice().as_ptr() as usize;

        // They should be adjacent (buddies)
        assert_eq!(ptr2.wrapping_sub(ptr1), 4096);

        drop(buf1);
        drop(buf2);

        // After dropping both, they should merge to 8KB order.
        // Alloc 8KB should get the same space (offset 0)
        let buf3 = pool.alloc(BufferSize::Size16K).unwrap(); // Wait 16KB Request requires 16KB.
        let buf_check = pool.alloc(BufferSize::Custom(8192)).unwrap();
        // The first 8KB block should be available.
        // If merge happened, order 1 (8KB) list has entry 0.
        // Or higher order has it.
        // Actually initially 16MB is free.
        // 4KB alloc splits 16MB -> ... -> 8KB -> 4KB, 4KB.
        // 8KB free list has buddy of 4KB?
        // Let's trace:
        // Alloc 4KB: 16M->8M...->8K->4K(0), 4K(4096). List: 4K(4096), 8K(8192)...
        // ptr1=0.
        // Alloc 4KB: takes 4K(4096). ptr2=4096.
        // Drop ptr1: 0 free. buddy 4096 is allocated. No merge. Free list 0: [0].
        // Drop ptr2: 4096 free. buddy 0 is free(order 0). Merge! -> 8K(0).
        // 8K(0) buddy 8K(8192) is free? Yes. Merge! -> 16K(0)... all way up.

        // So memory is fully merged back.
        // new Alloc 16KB should start at 0.
        // new Alloc 16KB should start at 0.
        // assert_eq!(buf_check.as_slice().as_ptr() as usize & 0xFFF, 0); // page aligned

        let ptr3 = buf3.as_slice().as_ptr() as usize;
        let ptr_check = buf_check.as_slice().as_ptr() as usize;
        // buf3 (16KB) is at 0. buf_check (8KB) is at 16KB (next slot).
        assert_eq!(ptr_check.wrapping_sub(ptr3), 16384);
    }
}
