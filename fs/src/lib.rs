#![no_std]
#![feature(drain_filter)]

extern crate alloc;

use alloc::{string::String, sync::Arc};
use block_cache::BlockCacheBuffer;
use block_dev::{
    BitmapBlock, BlockDevice, BlockId, DInode, InodeId, InodeType, SuperBlock, BLOCK_SIZE,
    DINODE_SIZE, INODES_PER_BLOCK,
};
use core::mem::size_of;
use inode::{Inode, InodeCacheBuffer, InodeNotExists};
use log::{debug, info, warn};
use spin::Mutex;

use crate::block_dev::{MAX_BLOCKS_ONE_INODE, MAX_SIZE_ONE_INODE};

pub mod block_cache;
pub mod block_dev;
mod file;
pub mod inode;

pub struct FileSystem {
    dev:         Arc<dyn BlockDevice>,
    // A copy of super block in memory.
    // We can't edit the data in super block on disk during the
    // file system running except when it creating. Therefor,
    // we can use it safely.
    pub sb:      Arc<SuperBlock>,
    // Synchronize access to disk blocks to ensure that only one
    // copy of a block in memory and that only one kernel thread
    // at a time use that copy.
    block_cache: Arc<Mutex<BlockCacheBuffer>>,
    // This lock protects the invariant that an inode is present in the
    // cache at most once.
    inode_cache: Arc<Mutex<InodeCacheBuffer>>,
}

impl FileSystem {
    pub fn create(
        dev: Arc<dyn BlockDevice>,
        total_blocks: u64,
        inode_blocks: u64,
    ) -> Result<Arc<Self>, FileSystemCreateError> {
        info!("fs: block size: {} bytes", BLOCK_SIZE);
        info!("fs: inode size: {} bytes", DINODE_SIZE);
        assert_eq!(
            DINODE_SIZE,
            BLOCK_SIZE / INODES_PER_BLOCK,
            "The size of the inode needs to be adapted to the `block_size`"
        );

        info!("fs: max blocks of one inode: {}", MAX_BLOCKS_ONE_INODE);
        info!(
            "fs: max data size of one inode: {} Bytes({} MB)",
            MAX_SIZE_ONE_INODE,
            MAX_SIZE_ONE_INODE / 1024
        );

        let super_blocks = 1;
        let logging_blocks = 1;

        let inode_bmap_blocks = inode_blocks / (size_of::<BitmapBlock>() as u64) + 1;
        let inode_area = inode_bmap_blocks + inode_blocks;

        debug!("fs: total blocks: {}", total_blocks);
        debug!("fs: inode blocks: {}", inode_blocks);
        debug!("fs: inode bitmap blocks: {}", inode_bmap_blocks);

        assert!(
            total_blocks > super_blocks + logging_blocks + inode_area,
            "No more space for data blocks."
        );

        let data_area = total_blocks - super_blocks - logging_blocks - inode_area; // bitmap + data blocks
        let data_bmap_blocks = (data_area / (1 + 8 * BLOCK_SIZE as u64)) + 1;
        let data_blocks = data_area - data_bmap_blocks;

        debug!("fs: data blocks: {}", data_blocks);
        debug!("fs: data bitmap blocks: {}", data_bmap_blocks);

        let super_block_start = 1;
        let inode_bmap_start = 2;
        let inode_start = 3;
        let data_bmap_start = inode_start + inode_blocks;
        let data_start = data_bmap_start + data_bmap_blocks;

        let block_cache = Arc::new(Mutex::new(BlockCacheBuffer::new()));

        // Clear all non-data blocks.
        for i in 0..data_start {
            block_cache.lock().get(i, dev.clone()).lock().write(
                0,
                |data_block: &mut [u8; BLOCK_SIZE]| {
                    for b in data_block.iter_mut() {
                        *b = 0;
                    }
                },
            )
        }

        // Initialize the super block.
        block_cache
            .lock()
            .get(super_block_start, dev.clone())
            .lock()
            .write(0, |super_block: &mut SuperBlock| {
                super_block.initialize(
                    total_blocks,
                    data_blocks,
                    inode_blocks,
                    inode_bmap_start,
                    inode_start,
                    data_bmap_start,
                    data_start,
                );
            });

        block_cache.lock().flush();

        let fs = FileSystem::open(dev).expect("Failed to create file system.");

        // Create the root inode and initialize it.
        let root_inode = fs
            .allocate_inode(InodeType::Directory)
            .expect("Failed to create the root inode.");
        assert_eq!(root_inode.lock().inode_num, 0);

        block_cache.lock().flush();

        Ok(fs)
    }

    pub fn open(dev: Arc<dyn BlockDevice>) -> Result<Arc<Self>, FileSystemInvalid> {
        let block_cache = Arc::new(Mutex::new(BlockCacheBuffer::new()));
        let inode_cache = Arc::new(Mutex::new(InodeCacheBuffer::new()));

        let mut lock = block_cache.lock();
        lock.get(1, dev.clone())
            .lock()
            .read(0, |super_block: &SuperBlock| {
                if super_block.is_valid() {
                    Ok(Arc::new(Self {
                        dev:         dev.clone(),
                        sb:          Arc::new(super_block.clone()),
                        block_cache: block_cache.clone(),
                        inode_cache: inode_cache.clone(),
                    }))
                } else {
                    Err(FileSystemInvalid())
                }
            })
    }

    /// Allocates a new empty inode from current file system.
    pub fn allocate_inode(self: &Arc<Self>, type_: InodeType) -> Option<Arc<Mutex<Inode>>> {
        if let Some(inum) = {
            let mut block_cache = self.block_cache.lock();
            block_cache
                .get(self.sb.inode_bmap_start, self.dev.clone())
                .lock()
                .write(0, |inode_bmap: &mut BitmapBlock| inode_bmap.allocate())
            // Release the lock of `block_cache` here.
        } {
            // The `inum` may be exceeding the limits of maximum number
            // of inodes, so we can't use it directly.
            if inum > self.max_inode_num() as usize {
                warn!("Failed to allocate an inode: the new `inum` exceeds the max inum of inode.");
                warn!("inum: {}, max_inum: {}", inum, self.max_inode_num());
                return None;
            }

            match self.inode_cache.lock().get(inum as InodeId, self.clone()) {
                Ok(inode) => {
                    inode
                        .lock()
                        .update_dinode(|dinode| dinode.initialize(type_));
                    Some(inode)
                }
                _ => panic!("Failed to access the inode just allocated: {}", inum),
            }
        } else {
            warn!("Failed to allocate an inode: exceeding the range of inode bit map.");
            None
        }
    }

    /// Allocates a free space in data area.
    pub fn allocate_block(self: &Arc<Self>) -> Option<BlockId> {
        if let Some(block_offset) = self
            .block_cache
            .lock()
            .get(self.sb.data_bmap_start, self.dev.clone())
            .lock()
            .write(0, |data_bmap: &mut BitmapBlock| data_bmap.allocate())
        {
            Some(self.sb.data_start + block_offset as BlockId)
        } else {
            None
        }
    }

    /// Gets the root inode.
    ///
    /// # Safety
    /// Panics when the root inode has not been created.
    pub fn root(self: &Arc<Self>) -> Arc<Mutex<Inode>> {
        self.get_inode(0).unwrap()
    }

    fn get_inode(self: &Arc<Self>, inum: InodeId) -> Result<Arc<Mutex<Inode>>, InodeNotExists> {
        self.inode_cache.lock().get(inum, self.clone())
    }

    fn max_inode_num(self: &Arc<Self>) -> InodeId {
        self.sb.inode_blocks_num * (INODES_PER_BLOCK as u64)
    }
}

#[derive(Debug)]
pub struct FileSystemCreateError();

#[derive(Debug)]
pub struct FileSystemInvalid();

#[derive(Debug)]
pub enum FileSystemAllocationError {
    Exhausted(usize),
    InodeExhausted,
    AlreadyExist(String, InodeType),
    TooLarge(usize),
}
