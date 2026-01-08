use super::{AlignedMemory, AllocError, AllocResult, BufPool, DeallocParams, FixedBuf, PoolVTable};
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

// Buddy System Constants
const ARENA_SIZE: usize = 32 * 1024 * 1024; // 32MB Total to support higher concurrency with overhead
const MIN_BLOCK_SIZE: usize = 4096; // 4KB to support 4KB payload with 4KB alignment

// Number of orders: 4KB, 8KB, 16KB, 32KB, 64KB, 128KB, 256KB, 512KB, 1MB, 2MB, 4MB, 8MB, 16MB, 32MB
const NUM_ORDERS: usize = 14;

const TAG_ALLOCATED: u8 = 0x80;
const TAG_ORDER_MASK: u8 = 0x7F;

/// 侵入式双向链表节点，存储在空闲块的头部
#[repr(C)]
struct FreeNode {
    prev: Option<NonNull<FreeNode>>,
    next: Option<NonNull<FreeNode>>,
}

/// 核心分配器逻辑，管理内存块和空闲列表
/// 独立于 BufPool trait，便于单独测试
struct BuddyAllocator {
    // 保持对内存的所有权
    _memory_owner: AlignedMemory,
    // 内存基地址，用于指针计算
    base_ptr: *mut u8,

    // 每个阶数（Order）对应的空闲链表头
    free_heads: [Option<NonNull<FreeNode>>; NUM_ORDERS],

    // 块标签数组，索引为 block_offset / 4096
    // 记录块的 Order 和是否已分配状态
    tags: Vec<u8>,
}

impl BuddyAllocator {
    fn new() -> Result<Self, AllocError> {
        // 分配 Arena 内存
        let memory = AlignedMemory::new(ARENA_SIZE, 4096)?;
        let base_ptr = memory.as_ptr();

        let mut free_heads = [None; NUM_ORDERS];

        // 初始化最大的块（Order 12, 16MB）
        let max_order = NUM_ORDERS - 1;

        // SAFETY: 刚刚分配的内存，指针有效且大小足够
        let root_node_ptr = unsafe { NonNull::new_unchecked(base_ptr as *mut FreeNode) };
        unsafe {
            *(base_ptr as *mut FreeNode) = FreeNode {
                prev: None,
                next: None,
            };
        }

        free_heads[max_order] = Some(root_node_ptr);

        let leaf_count = ARENA_SIZE / MIN_BLOCK_SIZE;
        let mut tags = vec![0u8; leaf_count];
        // 标记第一个最大块为空闲
        tags[0] = max_order as u8;

        Ok(Self {
            _memory_owner: memory,
            base_ptr,
            free_heads,
            tags,
        })
    }

    fn calculate_order(size: usize) -> Option<usize> {
        if size > ARENA_SIZE {
            return None;
        }
        if size <= MIN_BLOCK_SIZE {
            return Some(0);
        }
        // MIN_BLOCK_SIZE is 4096 (2^12)
        let order = size.next_power_of_two().ilog2() as usize - 12;
        if order >= NUM_ORDERS {
            None
        } else {
            Some(order)
        }
    }

    /// 分配指定大小的内存块
    /// 返回 (pointer, order)
    fn alloc(&mut self, size: usize) -> Option<(NonNull<u8>, usize)> {
        let needed_order = Self::calculate_order(size)?;

        // 寻找合适的空闲块
        for order in needed_order..NUM_ORDERS {
            if let Some(node_ptr) = self.free_heads[order] {
                // 1. 从链表移除空闲块
                // SAFETY: node_ptr 来源于 free_heads，保证有效且属于该 order
                unsafe { self.pop_front(order, node_ptr) };

                let curr_ptr = node_ptr.as_ptr() as *mut u8;
                let mut curr_order = order;
                let base_ptr = self.base_ptr;

                // 2. 迭代分裂直到达到所需大小
                while curr_order > needed_order {
                    curr_order -= 1;
                    let block_size = MIN_BLOCK_SIZE << curr_order;

                    // Buddy 是高地址的那一半
                    // SAFETY: 向下分裂时，block_size 必定在当前块范围内
                    let buddy_ptr = unsafe { curr_ptr.add(block_size) };

                    // 将 Buddy 初始化为 FreeNode 并加入对应的空闲链表
                    // SAFETY: buddy_ptr 指向有效的未使用内存
                    unsafe {
                        self.push_front(
                            curr_order,
                            NonNull::new_unchecked(buddy_ptr as *mut FreeNode),
                        )
                    };

                    // 更新 Buddy 的 Tag
                    // SAFETY: 都在 Arena 范围内
                    let buddy_offset = unsafe { buddy_ptr.offset_from(base_ptr) } as usize;
                    let buddy_idx = buddy_offset / MIN_BLOCK_SIZE;
                    self.tags[buddy_idx] = curr_order as u8;
                }

                // 3. 标记分配出的块
                // SAFETY: 指针在 Arena 范围内
                let offset = unsafe { curr_ptr.offset_from(base_ptr) } as usize;
                let idx = offset / MIN_BLOCK_SIZE;
                self.tags[idx] = (needed_order as u8) | TAG_ALLOCATED;

                return Some((
                    // SAFETY: curr_ptr 是有效的非空指针
                    unsafe { NonNull::new_unchecked(curr_ptr) },
                    needed_order,
                ));
            }
        }
        None
    }

    /// 释放内存块
    unsafe fn dealloc(&mut self, ptr: NonNull<u8>, order: usize) {
        let base_ptr = self.base_ptr;
        let ptr_raw = ptr.as_ptr();
        // SAFETY: 假定 ptr 是由 alloc 返回的，在 Arena 范围内
        let offset = unsafe { ptr_raw.offset_from(base_ptr) } as usize;

        let mut curr_offset = offset;
        let mut curr_order = order;

        // 立即标记为空闲
        let idx = curr_offset / MIN_BLOCK_SIZE;
        self.tags[idx] = curr_order as u8;

        // 尝试向上合并
        while curr_order < NUM_ORDERS - 1 {
            let block_size = MIN_BLOCK_SIZE << curr_order;
            let buddy_offset = curr_offset ^ block_size;

            if buddy_offset >= ARENA_SIZE {
                break;
            }

            let buddy_idx = buddy_offset / MIN_BLOCK_SIZE;
            let buddy_tag = self.tags[buddy_idx];

            // 检查 Buddy 是否空闲且 Order 一致
            if (buddy_tag & TAG_ALLOCATED) == 0 && (buddy_tag & TAG_ORDER_MASK) == curr_order as u8
            {
                // 合并 Buddy
                // SAFETY: buddy_offset 经过检查在 Arena 范围内
                let buddy_ptr = unsafe { base_ptr.add(buddy_offset) } as *mut FreeNode;
                // SAFETY: buddy_ptr 非空
                let buddy_non_null = unsafe { NonNull::new_unchecked(buddy_ptr) };

                // 从空闲链表中移除 Buddy
                // SAFETY: buddy 是空闲块，必定在链表中
                unsafe { self.remove(curr_order, buddy_non_null) };

                // 更新为合并后的大块
                curr_offset = std::cmp::min(curr_offset, buddy_offset);
                curr_order += 1;

                // 更新新块的 Tag
                let new_idx = curr_offset / MIN_BLOCK_SIZE;
                self.tags[new_idx] = curr_order as u8;
            } else {
                break;
            }
        }

        // 将最终的空闲块加入链表
        // SAFETY: curr_offset 始终在 Arena 范围内
        let final_ptr = unsafe { base_ptr.add(curr_offset) } as *mut FreeNode;
        // SAFETY: final_ptr 有效
        unsafe { self.push_front(curr_order, NonNull::new_unchecked(final_ptr)) };
    }

    // --- 链表操作辅助函数 ---

    unsafe fn push_front(&mut self, order: usize, mut node_ptr: NonNull<FreeNode>) {
        let head = self.free_heads[order];
        let node = unsafe { node_ptr.as_mut() };

        node.next = head;
        node.prev = None;

        if let Some(mut head_ptr) = head {
            unsafe { head_ptr.as_mut() }.prev = Some(node_ptr);
        }

        self.free_heads[order] = Some(node_ptr);
    }

    unsafe fn pop_front(&mut self, order: usize, mut node_ptr: NonNull<FreeNode>) {
        let node = unsafe { node_ptr.as_mut() };
        let next = node.next;

        self.free_heads[order] = next;

        if let Some(mut next_ptr) = next {
            unsafe { next_ptr.as_mut() }.prev = None;
        }

        // 清理指针是个好习惯
        node.next = None;
        node.prev = None;
    }

    unsafe fn remove(&mut self, order: usize, mut node_ptr: NonNull<FreeNode>) {
        let node = unsafe { node_ptr.as_mut() };
        let prev = node.prev;
        let next = node.next;

        if let Some(mut prev_ptr) = prev {
            unsafe { prev_ptr.as_mut() }.next = next;
        } else {
            // 是头节点
            self.free_heads[order] = next;
        }

        if let Some(mut next_ptr) = next {
            unsafe { next_ptr.as_mut() }.prev = prev;
        }

        node.prev = None;
        node.next = None;
    }

    // --- 测试辅助函数 ---
    #[cfg(test)]
    fn count_free(&self, order: usize) -> usize {
        let mut count = 0;
        let mut curr = self.free_heads[order];
        unsafe {
            while let Some(node) = curr {
                count += 1;
                curr = node.as_ref().next;
            }
        }
        count
    }
}

/// 各种 BufferPool 实现的包装器
#[derive(Clone)]
pub struct BuddyPool {
    inner: Rc<RefCell<BuddyAllocator>>,
}

impl std::fmt::Debug for BuddyPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuddyPool").finish_non_exhaustive()
    }
}

// Static VTable for Type Erasure
static BUDDY_POOL_VTABLE: PoolVTable = PoolVTable {
    dealloc: buddy_dealloc_shim,
    resolve_region_info: buddy_resolve_region_info_shim,
};

unsafe fn buddy_dealloc_shim(pool_data: NonNull<()>, params: DeallocParams) {
    // 1. Recover the Pool Rc
    let pool_rc = unsafe { Rc::from_raw(pool_data.as_ptr() as *const RefCell<BuddyAllocator>) };
    // Rc is now alive again (and will drop at end of scope, decrementing validity)

    // 2. Adjust params to find original block start
    let block_start_ptr = params.ptr.as_ptr();
    let block_start_non_null = unsafe { NonNull::new_unchecked(block_start_ptr) };

    // 3. Recover size from context (Buddy uses Order in context)
    let order = params.context;

    // 4. Perform dealloc
    let mut inner = pool_rc.borrow_mut();
    unsafe { inner.dealloc(block_start_non_null, order) };
}

unsafe fn buddy_resolve_region_info_shim(pool_data: NonNull<()>, buf: &FixedBuf) -> (usize, usize) {
    let raw = pool_data.as_ptr() as *const RefCell<BuddyAllocator>;
    let rc = std::mem::ManuallyDrop::new(unsafe { Rc::from_raw(raw) });
    let inner = rc.borrow();
    (
        0,
        (buf.as_ptr() as usize).saturating_sub(inner.base_ptr as usize),
    )
}

impl BuddyPool {
    pub fn new() -> Result<Self, AllocError> {
        Ok(Self {
            inner: Rc::new(RefCell::new(BuddyAllocator::new()?)),
        })
    }

    pub fn alloc(&self, len: usize) -> Option<FixedBuf> {
        let mut inner = self.inner.borrow_mut();

        if let Some((block_ptr, order)) = inner.alloc(len) {
            let capacity = MIN_BLOCK_SIZE << order;

            unsafe {
                let mut buf = FixedBuf::new(
                    block_ptr,
                    capacity,
                    0, // Global index is 0
                    self.pool_data(),
                    self.vtable(),
                    order, // Use order as context
                );
                buf.set_len(len);
                Some(buf)
            }
        } else {
            None
        }
    }
}

impl BufPool for BuddyPool {
    fn alloc_mem(&self, size: usize) -> AllocResult {
        let mut inner = self.inner.borrow_mut();

        match inner.alloc(size) {
            Some((block_ptr, order)) => {
                let capacity = MIN_BLOCK_SIZE << order;
                // No header writing needed

                AllocResult::Allocated {
                    ptr: block_ptr,
                    cap: capacity,
                    global_index: 0,
                    context: order,
                }
            }
            None => AllocResult::Failed,
        }
    }

    fn get_memory_regions(&self) -> Vec<crate::io::buffer::BufferRegion> {
        let inner = self.inner.borrow();
        vec![crate::io::buffer::BufferRegion {
            ptr: unsafe { NonNull::new_unchecked(inner.base_ptr as *mut _) },
            len: ARENA_SIZE,
        }]
    }

    fn vtable(&self) -> &'static PoolVTable {
        &BUDDY_POOL_VTABLE
    }

    fn pool_data(&self) -> NonNull<()> {
        unsafe {
            let raw = Rc::into_raw(self.inner.clone());
            NonNull::new_unchecked(raw as *mut ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alloc_basic() {
        let mut allocator = BuddyAllocator::new().unwrap();
        // 初始状态：1个 MaxOrder 块 (Order 13)
        assert_eq!(allocator.count_free(NUM_ORDERS - 1), 1);

        // 分配 4KB (Order 0)
        let (ptr1, order1) = allocator.alloc(4096).unwrap();
        assert_eq!(order1, 0);

        // 分裂路径验证
        // MaxOrder -> ... -> 8K -> 4K(Allocated) + 4K(Free)
        // 所有的中间级 (4K ... MaxOrder) 都应该各有一个 Free 块
        assert_eq!(allocator.count_free(0), 1); // 剩下一个 4K
        assert_eq!(allocator.count_free(1), 1); // 剩下一个 8K
        assert_eq!(allocator.count_free(NUM_ORDERS - 1), 0); // MaxOrder 没了

        unsafe { allocator.dealloc(ptr1, order1) };

        // 释放后应完全合并
        assert_eq!(allocator.count_free(NUM_ORDERS - 1), 1);
        assert_eq!(allocator.count_free(0), 0);
    }

    #[test]
    fn test_pool_integration() {
        let pool = BuddyPool::new().unwrap();
        // With 4KB alignment, a 4K block has 4096 capacity.
        // Minimal usable block is 4K.
        let buf = pool.alloc(4096).unwrap();
        // Capacity should be 4096
        assert_eq!(buf.capacity(), 4096);
        drop(buf);
        // Ensure no panic on drop and proper rc cleanup
    }
}
