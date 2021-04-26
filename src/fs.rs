use crate::{
  array_vec::ArrayVec,
  bit_array::{nearest_div_8, BitArray},
  block_interface::{
    AllMetadata, BlockDevice, GlobalBlockInterface, Metadata, MetadataHandle, Owner,
    FIRST_FREE_BLOCK, OWN_BLOCKS,
  },
  default_ser_impl,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FileDescriptor(u32);

impl From<u32> for FileDescriptor {
  fn from(v: u32) -> Self { FileDescriptor(v) }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FileMode {
  R = 1,
  W = 2,
  RW = 3,
  /// Attempts to create a file, errors if already exists.
  /// Functionally equivalent to RW otherwise.
  New = 4,
  // TODO maybe make this into a u8 and instead use a bunch of flags, also need one for
  // existence
  MustExist = 8,
}

impl Default for FileMode {
  fn default() -> Self { Self::R }
}

impl core::str::FromStr for FileMode {
  type Err = ();
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    use FileMode::*;
    let kind = match s {
      "R" => R,
      "W" => W,
      "RW" => RW,
      "New" => New,
      "MustExist" => MustExist,
      _ => return Err(()),
    };
    Ok(kind)
  }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum SeekFrom {
  Start(u32),
  End(i32),
  Current(i32),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub struct FileDescEntry {
  inode: u32,
  offset: u32,
  open_refs: u16,
  mode: FileMode,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub struct FileStat {
  size: u32,
}

/// Number of entries inside of a directory
const NUM_ENTRIES: usize = 64;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Directory {
  parent_inode: u32,
  own_inode: u32,
  pub(crate) num_added: u8,
  name_to_inode_map: [([u8; 48], u32); NUM_ENTRIES],
}

impl Directory {
  fn new(own_inode: u32, parent_inode: u32) -> Self {
    Self {
      parent_inode,
      own_inode,
      num_added: 0,
      // The last byte in this array is the length, which is super hacky.
      name_to_inode_map: [([0; 48], 0); NUM_ENTRIES],
    }
  }

  /// Returns all the byte array entries of this directory
  #[inline]
  pub fn entries(&self) -> impl Iterator<Item = (&[u8], u32)> {
    core::iter::once((b".".as_slice(), self.own_inode))
      .chain(core::iter::once((b"..".as_slice(), self.parent_inode)))
      .chain(
        self
          .name_to_inode_map
          .iter()
          .take(self.num_added as usize)
          .map(|(name, inode)| (&name[..name[47] as usize], *inode)),
      )
  }

  /// Inserts an entry into this directory
  pub fn insert(&mut self, name: &str, to: u32) -> Result<(), ()> {
    if self.num_added as usize >= NUM_ENTRIES {
      // TODO directory out of space
      return Err(());
    }
    if self.contains(name) {
      // TODO already contains file with that name
      return Err(());
    }
    let bytes = name.as_bytes();
    assert!(bytes.len() < 48, "Cannot store names this long currently.");
    let len = bytes.len();
    let idx = self.num_added as usize;
    self.name_to_inode_map[idx].0[..len].copy_from_slice(bytes);
    self.name_to_inode_map[idx].0[47] = len as u8;
    self.name_to_inode_map[idx].1 = to;
    self.num_added += 1;
    Ok(())
  }

  /// Finds an entry in the directory
  pub fn inode_of(&self, name: &str) -> Option<u32> {
    if "." == name {
      return Some(self.own_inode);
    } else if ".." == name {
      return Some(self.parent_inode);
    }
    let bytes = name.as_bytes();
    if bytes.len() > 47 {
      return None;
    }
    self
      .name_to_inode_map
      .iter()
      .take(self.num_added as usize)
      .find_map(|(fname, inode)| fname[..fname[47] as usize].eq(bytes).then(|| *inode))
  }
  #[inline]
  pub fn contains(&mut self, name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() > 47 {
      return false;
    }
    "." == name
      || ".." == name
      || self
        .name_to_inode_map
        .iter()
        .take(self.num_added as usize)
        .any(|(fname, _inode)| fname[..fname[47] as usize].eq(bytes))
  }
  /// Removes an entry from this directory
  pub fn remove(&mut self, entry_num: usize) {
    if entry_num > self.num_added as usize {
      return;
    }
    self
      .name_to_inode_map
      .swap(entry_num, (self.num_added - 1) as usize);
    self.num_added -= 1;
  }
  pub fn index_of(&self, name: &str) -> Option<usize> {
    let bytes = name.as_bytes();
    if bytes.len() > 47 || name == "." || name == ".." {
      return None;
    }
    self
      .name_to_inode_map
      .iter()
      .take(self.num_added as usize)
      .position(|(fname, _inode)| fname[..fname[47] as usize].eq(bytes))
  }
  #[inline]
  pub const fn is_empty(&self) -> bool { self.num_added == 0 }
  default_ser_impl!();
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct INode {
  /// Number of links to this inode
  refs: u16,
  kind: INodeKind,
  size: u32,
  // TODO think of a way to extend the size of data blocks here
  data_blocks: [u16; 8],
}

impl INode {
  const fn new(kind: INodeKind) -> Self {
    INode {
      refs: 1,
      kind,
      size: 0,
      data_blocks: [0; 8],
    }
  }
  const fn num_data_blocks(&self, block_size: u32) -> u32 {
    (self.size + (block_size - 1)) / block_size
  }
  /// Creates an inode from a slice of bytes
  #[inline]
  fn from_slice(s: &[u8]) -> Self {
    assert_eq!(s.len(), core::mem::size_of::<Self>());
    let mut data_blocks = [0u16; 8];
    for i in 0..8 {
      data_blocks[i] = u16::from_ne_bytes([s[7 + i * 2], s[8 + i * 2]]);
    }
    Self {
      refs: u16::from_ne_bytes([s[0], s[1]]),
      kind: INodeKind::from(s[2]),
      size: u32::from_ne_bytes([s[3], s[4], s[5], s[6]]),
      data_blocks,
    }
  }
  fn to_slice(&self, dst: &mut [u8]) {
    assert_eq!(dst.len(), core::mem::size_of::<Self>());
    dst[..2].copy_from_slice(&self.refs.to_ne_bytes());
    dst[2] = self.kind as u8;
    dst[3..7].copy_from_slice(&self.size.to_ne_bytes());
    for i in 0..8 {
      let [l, r] = self.data_blocks[i].to_ne_bytes();
      dst[7 + i * 2] = l;
      dst[8 + i * 2] = r;
    }
  }
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum INodeKind {
  File = 0,
  Directory = 1,
  Contiguous = 2,
}

impl From<u8> for INodeKind {
  fn from(v: u8) -> Self {
    match v {
      0 => INodeKind::File,
      1 => INodeKind::Directory,
      2 => INodeKind::Contiguous,
      v => panic!("Unknown INodeKind {}", v),
    }
  }
}

const MAGIC_NUMBER: u32 = 0x101_0_F_0ff;
/// The superblock is where the block alloc maps are stored
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
  magic_number: u32,
  owned: Option<[u32; 1]>,
}

impl Metadata for Superblock {
  fn new() -> Self {
    Superblock {
      magic_number: MAGIC_NUMBER,
      owned: None,
    }
  }
  type Iter<'a> = core::iter::Copied<core::slice::Iter<'a, u32>>;
  fn owned(&self) -> Self::Iter<'_> {
    match self.owned {
      None => [].iter(),
      Some(ref v) => v.iter(),
    }
    .copied()
  }
  fn insert(&self, b: u32) -> Result<Self, ()> {
    if self.owned.is_some() {
      return Err(());
    }
    Ok(Self {
      owned: Some([b]),
      ..*self
    })
  }
  fn remove(&self, b: u32) -> Result<Self, ()> {
    if self.owned != Some([b]) {
      return Err(());
    }
    Ok(Self {
      owned: None,
      ..*self
    })
  }
  const LEN: usize = core::mem::size_of::<Self>();
  default_ser_impl!();
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeMetadata {
  start: u32,
  count: u32,
}

impl Metadata for RangeMetadata {
  fn new() -> Self { RangeMetadata { start: 0, count: 0 } }
  type Iter<'a> = core::ops::Range<u32>;
  fn owned(&self) -> Self::Iter<'_> { self.start..self.start + self.count }
  const LEN: usize = core::mem::size_of::<Self>();
  fn insert(&self, b: u32) -> Result<Self, ()> {
    if self.count == 0 {
      return Ok(Self { start: b, count: 1 });
    }
    // Handle an extra block after the end of this one
    if b == self.start + self.count {
      return Ok(Self {
        start: self.start,
        count: self.count + 1,
      });
    } else if b == self.start.checked_sub(1).ok_or(())? {
      return Ok(Self {
        start: self.start - 1,
        count: self.count + 1,
      });
    }
    Err(())
  }

  fn remove(&self, _b: u32) -> Result<Self, ()> {
    todo!();
  }

  default_ser_impl!();
}

/// The number of inode blocks in this system.
pub const NUM_INODE: usize = 512;
/// The number of data blocks in this system.
pub const NUM_DATA: usize = 1024;
// TODO for some reason I can't just plug it in below so I need to hard code this

/// A singleton file system type.
pub struct FileSystem<'a, B: 'a + BlockDevice>
where
  B: BlockDevice,
  [(); nearest_div_8(B::NUM_BLOCKS)]: ,
  [(); B::BLOCK_SIZE]: , {
  superblock: MetadataHandle,
  file_descs: [FileDescEntry; 256],
  /// Keeps track of how many file descriptors there are for a specific inode.
  open_counts: [u8; NUM_INODE],
  inode_md: MetadataHandle,
  data_md: MetadataHandle,

  /// A cache for commonly written inodes so we don't have to go to disk everytime.
  inode_cache: ArrayVec<(INode, u32), 4>,

  // These are pub(crate) so that they can be looked at.
  pub(crate) inode_alloc_map: BitArray<512>,
  pub(crate) data_alloc_map: BitArray<1024>,
  pub gbi: &'a mut GlobalBlockInterface<B>,
}

impl<'a, B> FileSystem<'a, B>
where
  B: BlockDevice + 'a,
  [(); nearest_div_8(B::NUM_BLOCKS)]: ,
  [(); B::BLOCK_SIZE]: ,
  [(); 2 * B::BLOCK_SIZE]: ,
  [(); OWN_BLOCKS * B::BLOCK_SIZE]: ,
{
  pub fn new(gbi: &'a mut GlobalBlockInterface<B>) -> Self {
    let sb_mh = gbi
      .metadatas_for(Owner::LibFS)
      .find_map(|(mh, amd)| matches!(amd, AllMetadata::Superblock(_)).then(|| mh));
    if let Some(sb_mh) = sb_mh {
      // File system previously existed, reinitialize
      let mut mhs = gbi
        .metadatas_for(Owner::LibFS)
        .filter(|(_, amd)| matches!(amd, AllMetadata::RangeMetadata(_)))
        .map(|v| v.0);
      let inode_mh = mhs.next().expect("Failed to get inode metadata handle");
      let data_mh = mhs.next().expect("Failed to get data metadata handle");
      assert!(mhs.next().is_none());
      drop(mhs);
      let mut out = Self {
        superblock: sb_mh,
        inode_md: inode_mh,
        data_md: data_mh,
        open_counts: [0; NUM_INODE],
        inode_cache: ArrayVec::new(),
        inode_alloc_map: BitArray::new(false),
        data_alloc_map: BitArray::new(false),
        file_descs: [FileDescEntry::default(); 256],
        gbi,
      };
      out.load_allocs().expect("Failed to load alloc map");
      out
    } else {
      Self::mk(gbi)
    }
  }
  /// Makes a new instance of this file system on this block interface
  fn mk(gbi: &'a mut GlobalBlockInterface<B>) -> Self {
    let sb_mh = gbi
      .new_metadata::<Superblock>(Owner::LibFS)
      .expect("Failed to mk sb md");
    gbi
      .req_block(sb_mh, FIRST_FREE_BLOCK)
      .expect("Failed to initialize Super block");
    let inode_mh = gbi
      .new_metadata::<RangeMetadata>(Owner::LibFS)
      .expect("Failed to mk inode md");
    let mut curr = FIRST_FREE_BLOCK + 1;
    let num_inode_blocks = Self::num_inode_blocks() as u32;
    for i in curr..curr + num_inode_blocks {
      gbi
        .req_block(inode_mh, i)
        .expect("Failed to add block to inode");
    }
    curr += num_inode_blocks;
    let data_mh = gbi
      .new_metadata::<RangeMetadata>(Owner::LibFS)
      .expect("Failed to mk data md");
    for i in curr..curr + NUM_DATA as u32 {
      gbi
        .req_block(data_mh, i)
        .expect("Failed to add block to data");
    }
    gbi
      .persist()
      .expect("Failed to persist global block interface");
    let mut out = Self {
      superblock: sb_mh,
      inode_md: inode_mh,
      data_md: data_mh,
      inode_cache: ArrayVec::new(),
      inode_alloc_map: BitArray::new(false),
      data_alloc_map: BitArray::new(false),
      file_descs: [FileDescEntry::default(); 256],
      open_counts: [0; NUM_INODE],
      gbi,
    };
    // make root directory at inode 0.
    let mut root_dir_inode = INode::new(INodeKind::Directory);
    let root_dir_inode_num = out
      .alloc_inode(&root_dir_inode)
      .expect("Failed to allocate inode for root directory");
    assert_eq!(root_dir_inode_num, 0, "Unexpected inode 0 for root dir");
    let root_dir = Directory::new(0, 0);
    // Writes the root dir to the first inode
    out
      .write_to_inode(&mut root_dir_inode, root_dir.ser(), 0)
      .expect("Failed to write root dir to inode");
    out
      .save_inode(&root_dir_inode, root_dir_inode_num as usize)
      .expect("Failed to save root dir inode");
    out
  }
  /// Opens a file to the root directory of the file system.
  pub fn root_dir(&mut self, mode: FileMode) -> Result<FileDescriptor, ()> {
    let (i, fd) = self
      .file_descs
      .iter_mut()
      .enumerate()
      .find(|(_, fd)| fd.open_refs == 0)
      .ok_or(())?;
    fd.inode = 0;
    fd.open_refs += 1;
    fd.offset = 0;
    fd.mode = mode;
    self.open_counts[fd.inode as usize] += 1;
    Ok(FileDescriptor(i as u32))
  }

  /// Flushes the cache to ensure that all writes are persisted.
  pub fn flush(&mut self) -> Result<(), FlushErr> {
    while let Some((inode, inode_num)) = self.inode_cache.pop() {
      self.save_inode(&inode, inode_num as usize)?;
    }
    Ok(())
  }
  /// Seeks inside of a file
  pub fn seek(&mut self, FileDescriptor(fdi): FileDescriptor, s: SeekFrom) -> Result<(), SeekErr> {
    let fdi = fdi as usize;
    let fd = self.file_descs.get_mut(fdi).ok_or(FileDescOOB::OOB)?;
    match s {
      SeekFrom::Start(v) => fd.offset = v,
      SeekFrom::End(v) => {
        let inode_num = fd.inode;
        drop(fd);
        let inode = self.load_inode(inode_num as usize)?;
        let dst = inode.size as i32 + v;
        if dst < 0 {
          return Err(SeekErr::BeforeStart);
        }
        self.file_descs[fdi].offset = dst as u32;
      },
      SeekFrom::Current(v) => {
        let dst = fd.offset as i32 + v;
        if dst < 0 {
          return Err(SeekErr::BeforeStart);
        }
        fd.offset = dst as u32;
      },
    }
    Ok(())
  }
  /// Reads into dst from a given file.
  pub fn read(
    &mut self,
    FileDescriptor(fdi): FileDescriptor,
    dst: &mut [u8],
  ) -> Result<usize, ReadErr> {
    let fdi = fdi as usize;
    let fd = self.file_descs.get(fdi).ok_or(FileDescOOB::OOB)?;
    let offset = fd.offset;
    if fd.mode == FileMode::W {
      return Err(ReadErr::NotReadable);
    }
    let inode_num = fd.inode as usize;
    let inode = self.load_inode(inode_num)?;
    if inode.kind == INodeKind::Directory {
      assert_eq!(dst.len(), core::mem::size_of::<Directory>());
    }
    let read = self.read_from_inode(&inode, dst, offset)?;
    self.file_descs[fdi].offset += read as u32;
    Ok(read)
  }
  pub fn write(
    &mut self,
    FileDescriptor(fdi): FileDescriptor,
    src: &[u8],
  ) -> Result<usize, WriteErr> {
    let fdi = fdi as usize;
    let fd = self.file_descs.get(fdi).ok_or(FileDescOOB::OOB)?;
    if fd.mode == FileMode::R {
      return Err(WriteErr::ReadOnly);
    }
    let offset = fd.offset;
    let inode_num = fd.inode as usize;
    let mut inode = self.load_inode(inode_num)?;
    if inode.kind == INodeKind::Directory {
      assert_eq!(src.len(), core::mem::size_of::<Directory>());
    }
    let (written, updated) = self.write_to_inode(&mut inode, src, offset)?;
    if updated {
      self.cache_inode(&inode, inode_num)?;
    }
    self.file_descs[fdi].offset += written as u32;
    Ok(written)
  }
  pub fn stat(&mut self, FileDescriptor(fdi): FileDescriptor) -> Result<FileStat, StatErr> {
    let fdi = fdi as usize;
    let inode_num = self.file_descs.get(fdi).ok_or(FileDescOOB::OOB)?.inode as usize;
    let inode = self.load_inode(inode_num)?;
    Ok(FileStat { size: inode.size })
  }
  #[inline]
  const fn num_inode_blocks() -> usize {
    let mut num_blocks_for_inodes = NUM_INODE * core::mem::size_of::<INode>() / B::BLOCK_SIZE;
    if NUM_INODE * core::mem::size_of::<INode>() % B::BLOCK_SIZE != 0 {
      num_blocks_for_inodes += 1;
    }
    num_blocks_for_inodes
  }
  /// Returns the position of an inode and whether it wraps around to a second block
  #[inline]
  const fn inode_block_and_offset_and_wraps(i: usize) -> (usize, usize, bool) {
    // Should round down
    let bl = (i * core::mem::size_of::<INode>()) / B::BLOCK_SIZE;
    let next_bl = ((i + 1) * core::mem::size_of::<INode>()) / B::BLOCK_SIZE;
    let wraps = bl != next_bl;
    let offset = (i * core::mem::size_of::<INode>()) % B::BLOCK_SIZE;
    (bl, offset, wraps)
  }
  fn load_inode(&mut self, i: usize) -> Result<INode, LoadINodeErr> {
    debug_assert!(
      self.inode_alloc_map.get(i),
      "Unallocated inode is being loaded {}",
      i
    );
    if let Some((inode, _)) = self
      .inode_cache
      .as_slice()
      .iter()
      .find(|(_, num)| *num as usize == i)
    {
      return Ok(inode.clone());
    }
    let (block, offset, overlaps_end) = Self::inode_block_and_offset_and_wraps(i);
    if !overlaps_end {
      let mut buf = [0; B::BLOCK_SIZE];
      let inode_end = offset + core::mem::size_of::<INode>();
      self
        .gbi
        .read(self.inode_md, block, &mut buf[..inode_end])
        .map_err(|_| LoadINodeErr::RWErr)?;
      Ok(INode::from_slice(&buf[offset..inode_end]))
    } else {
      let mut buf = [0u8; 2 * B::BLOCK_SIZE];
      self
        .gbi
        .read(self.inode_md, block, &mut buf[..B::BLOCK_SIZE])
        .map_err(|_| LoadINodeErr::RWErr)?;
      self
        .gbi
        .read(self.inode_md, block + 1, &mut buf[B::BLOCK_SIZE..])
        .map_err(|_| LoadINodeErr::RWErr)?;
      Ok(INode::from_slice(
        &buf[offset..offset + core::mem::size_of::<INode>()],
      ))
    }
  }

  fn cache_inode(&mut self, inode: &INode, i: usize) -> Result<(), CacheINodeErr> {
    if let Some((prev, _)) = self
      .inode_cache
      .as_mut_slice()
      .iter_mut()
      .find(|(_, v)| *v as usize == i)
    {
      prev.clone_from(inode);
      return Ok(());
    }
    if let Some((old, old_i)) = self.inode_cache.push_out_front((*inode, i as u32)) {
      assert_ne!(old_i as usize, i);
      self.save_inode(&old, old_i as usize)?;
    }
    Ok(())
  }

  fn save_inode(&mut self, inode: &INode, i: usize) -> Result<(), SaveINodeErr> {
    assert!(
      self.inode_alloc_map.get(i as usize),
      "Unallocated inode is being saved"
    );
    self.inode_cache.remove_where(|(_, v)| *v as usize == i);
    let (block, offset, wraps) = Self::inode_block_and_offset_and_wraps(i);
    if !wraps {
      let mut buf = [0u8; B::BLOCK_SIZE];
      self
        .gbi
        .read(self.inode_md, block, &mut buf)
        .map_err(|_| SaveINodeErr::ReadFailed)?;
      inode.to_slice(&mut buf[offset..offset + core::mem::size_of::<INode>()]);
      self
        .gbi
        .write(self.inode_md, block, &buf)
        .map_err(|_| SaveINodeErr::WriteFailed)?;
    } else {
      let mut buf = [0u8; 2 * B::BLOCK_SIZE];
      self
        .gbi
        .read(self.inode_md, block, &mut buf[..B::BLOCK_SIZE])
        .map_err(|_| SaveINodeErr::ReadFailed)?;
      self
        .gbi
        .read(self.inode_md, block + 1, &mut buf[B::BLOCK_SIZE..])
        .map_err(|_| SaveINodeErr::ReadFailed)?;
      inode.to_slice(&mut buf[offset..offset + core::mem::size_of::<INode>()]);
      self
        .gbi
        .write(self.inode_md, block, &buf[..B::BLOCK_SIZE])
        .map_err(|_| SaveINodeErr::WriteFailed)?;
      self
        .gbi
        .write(self.inode_md, block + 1, &buf[B::BLOCK_SIZE..])
        .map_err(|_| SaveINodeErr::WriteFailed)?;
    }
    Ok(())
  }
  /// Given an inode, saves it and returns the inode number it was given
  fn alloc_inode(&mut self, inode: &INode) -> Result<u32, AllocINodeErr> {
    let inode_num = self
      .inode_alloc_map
      .find_free()
      .ok_or(AllocINodeErr::NoneFree)? as u32;
    self.inode_alloc_map.set(inode_num as usize);
    self.save_inode(inode, inode_num as usize)?;
    self.persist_allocs()?;
    Ok(inode_num)
  }

  #[inline]
  fn open_with_dir(
    &mut self,
    // Inside of which directory?
    FileDescriptor(fdi): FileDescriptor,
    path: &[&str],
    mode: FileMode,
  ) -> Result<(FileDescriptor, (u32, INode, Directory)), OpenErr> {
    if path.is_empty() {
      // Must have at least one entry in the path
      return Err(OpenErr::MustProvideFileName);
    }
    let mut curr_dir_inode_num = self
      .file_descs
      .get(fdi as usize)
      .ok_or(FileDescOOB::OOB)?
      .inode;
    let mut curr_dir_inode = self.load_inode(curr_dir_inode_num as usize)?;
    if curr_dir_inode.kind != INodeKind::Directory {
      // Can't create a file inside of something which isn't a directory
      return Err(OpenErr::NotInsideDirectory);
    }
    let mut buf = [0u8; core::mem::size_of::<Directory>()];
    self.read_from_inode(&curr_dir_inode, &mut buf, 0)?;
    let mut curr_dir: Directory = unsafe { core::mem::transmute(buf) };
    for subdir in path.iter().take(path.len() - 1) {
      curr_dir_inode_num = curr_dir.inode_of(subdir).ok_or(OpenErr::PathDoesNotExist)?;
      curr_dir_inode = self.load_inode(curr_dir_inode_num as usize)?;
      if curr_dir_inode.kind != INodeKind::Directory {
        return Err(OpenErr::NotInsideDirectory);
      }
      let mut buf: [u8; core::mem::size_of::<Directory>()] =
        unsafe { core::mem::transmute(curr_dir) };
      self.read_from_inode(&curr_dir_inode, &mut buf, 0)?;
      curr_dir = unsafe { core::mem::transmute(buf) };
    }
    let last_entry = path.last().unwrap();
    let inode_num = if let Some(inode_num) = curr_dir.inode_of(last_entry) {
      if mode == FileMode::New {
        // File already exists
        return Err(OpenErr::NewButFileExists);
      }
      if self.open_counts[inode_num as usize] == u8::MAX {
        // Already opened too many files
        return Err(OpenErr::FileOpenTooMuch);
      }
      inode_num
    } else if !matches!(mode, FileMode::R | FileMode::MustExist) {
      let inode = INode::new(INodeKind::File);
      let inode_num = self.alloc_inode(&inode)?;
      curr_dir
        .insert(last_entry, inode_num)
        .expect("Failed to insert name into directory");

      let (_, updated) = self.write_to_inode(&mut curr_dir_inode, curr_dir.ser(), 0)?;
      if updated {
        self.save_inode(&curr_dir_inode, curr_dir_inode_num as usize)?;
      }
      inode_num
    } else {
      debug_assert!(matches!(mode, FileMode::R | FileMode::MustExist));
      return Err(OpenErr::ModeCannotCreateFile);
    };
    self.open_counts[inode_num as usize] += 1;
    let (i, fd) = self
      .file_descs
      .iter_mut()
      .enumerate()
      .find(|(_, fd)| fd.open_refs == 0)
      .ok_or(OpenErr::NoFreeFDs)?;
    fd.inode = inode_num;
    fd.offset = 0;
    fd.open_refs += 1;
    fd.mode = mode;
    Ok((
      FileDescriptor(i as u32),
      (curr_dir_inode_num, curr_dir_inode, curr_dir),
    ))
  }

  /// Opens a given path relative to dir.
  pub fn open(
    &mut self,
    // Inside of which directory?
    fd: FileDescriptor,
    path: &[&str],
    mode: FileMode,
  ) -> Result<FileDescriptor, OpenErr> {
    self.open_with_dir(fd, path, mode).map(|v| v.0)
  }

  fn unlink_from_dir(
    &mut self,
    unlink_fd: FileDescriptor,
    curr_dir_inode_num: u32,
    mut curr_dir_inode: INode,
    mut curr_dir: Directory,
    last_entry: &str,
  ) -> Result<(), UnlinkErr> {
    let inode_num = self.file_descs[unlink_fd.0 as usize].inode as usize;
    let mut inode = self.load_inode(inode_num)?;

    curr_dir.remove(curr_dir.index_of(last_entry).unwrap());
    self.write_to_inode(&mut curr_dir_inode, curr_dir.ser(), 0)?;
    self.save_inode(&curr_dir_inode, curr_dir_inode_num as usize)?;

    inode.refs -= 1;
    self.save_inode(&inode, inode_num)?;
    self.close(unlink_fd)?;
    Ok(())
  }

  /// Unlinks a file in the directory given by the file descriptor, following the path.
  pub fn unlink(&mut self, fd: FileDescriptor, path: &[&str]) -> Result<(), UnlinkErr> {
    let (fd, (curr_dir_inode_num, curr_dir_inode, curr_dir)) =
      self.open_with_dir(fd, path, FileMode::R)?;
    self.unlink_from_dir(
      fd,
      curr_dir_inode_num,
      curr_dir_inode,
      curr_dir,
      path.last().unwrap(),
    )
  }
  pub fn close(&mut self, FileDescriptor(i): FileDescriptor) -> Result<(), CloseErr> {
    let i = i as usize;
    let open_file = &mut self.file_descs[i];
    if open_file.open_refs == 0 {
      return Err(CloseErr::NotOpen);
    }
    open_file.open_refs -= 1;
    // If inode is no longer referenced here delete it
    if open_file.open_refs == 0 {
      let inode_num = open_file.inode as usize;
      self.open_counts[inode_num] -= 1;
      if self.open_counts[inode_num] > 0 {
        return Ok(());
      }
      let mut inode = self.load_inode(inode_num)?;
      if inode.refs > 0 {
        return Ok(());
      }
      let n_db = inode.num_data_blocks(B::BLOCK_SIZE as u32) as usize;
      for d in inode.data_blocks.iter_mut().take(n_db) {
        self.data_alloc_map.unset(*d as usize);
        *d = 0;
      }
      inode.size = 0;
      self.save_inode(&inode, inode_num)?;
      self.inode_alloc_map.unset(inode_num);
      self.persist_allocs()?;
    }
    Ok(())
  }
  /// Saves the allocation maps to disk
  fn persist_allocs(&mut self) -> Result<(), PersistAllocsErr> {
    let mut buf = [0u8; { (NUM_INODE + NUM_DATA) / 8 }];
    assert!(buf.len() < B::BLOCK_SIZE);
    buf[..self.inode_alloc_map.items.len()].copy_from_slice(&self.inode_alloc_map.items);
    buf[self.inode_alloc_map.items.len()..].copy_from_slice(&self.data_alloc_map.items);
    self
      .gbi
      .write(self.superblock, 0, &buf)
      .map_err(|_| PersistAllocsErr::WriteErr)?;
    Ok(())
  }
  /// Loads the allocation maps from disk
  fn load_allocs(&mut self) -> Result<(), ()> {
    let mut buf = [0u8; { (NUM_INODE + NUM_DATA) / 8 }];
    self.gbi.read(self.superblock, 0, &mut buf)?;
    let len = self.inode_alloc_map.items.len();
    self.inode_alloc_map.items.copy_from_slice(&buf[..len]);
    self.data_alloc_map.items.copy_from_slice(&buf[len..]);
    Ok(())
  }

  /// Writes a byte array to an inode
  fn write_to_inode(
    &mut self,
    inode: &mut INode,
    data: &[u8],
    offset: u32,
  ) -> Result<(usize, bool), WriteToINodeErr> {
    let start_block = offset / (B::BLOCK_SIZE as u32);
    let end_byte = offset + data.len() as u32;
    let updated = inode.size < end_byte;
    let end_block = (end_byte as usize + B::BLOCK_SIZE - 1) / (B::BLOCK_SIZE);
    if end_block > 8 {
      // Not enough space in these files to write that much.
      // TODO lift this when I figure out a way to allocate more space
      return Err(WriteToINodeErr::NotEnoughSpace);
    }
    let curr_blocks = (inode.size as usize + B::BLOCK_SIZE - 1) / B::BLOCK_SIZE;
    // TODO check the number of free data blocks before and abort early if so;
    if curr_blocks < end_block {
      for i in curr_blocks..end_block {
        let free_db = self
          .data_alloc_map
          .find_free()
          .ok_or(WriteToINodeErr::NoDataBlocks)?;
        inode.data_blocks[i] = free_db as u16;
        assert!(!self.data_alloc_map.get(free_db));
        self.data_alloc_map.set(free_db);
      }
    }
    inode.size = inode.size.max(end_byte);
    let mut written = 0;
    let mut buf = [0; B::BLOCK_SIZE];
    for i in start_block..end_block as u32 {
      let db = inode.data_blocks[i as usize] as usize;
      let read = self
        .gbi
        .read(self.data_md, db, &mut buf)
        .map_err(|_| WriteToINodeErr::RWErr)?;
      debug_assert_eq!(read, B::BLOCK_SIZE);
      let start = (written + offset as usize) % B::BLOCK_SIZE;
      let end = B::BLOCK_SIZE.min(start + data.len() - written as usize);
      let write_buf = &mut buf[start..end];
      debug_assert_ne!(write_buf.len(), 0);
      write_buf.copy_from_slice(&data[written..written + write_buf.len()]);
      written += write_buf.len();
      self
        .gbi
        .write(self.data_md, db, &buf)
        .map_err(|_| WriteToINodeErr::RWErr)?;
    }
    debug_assert_eq!(written, data.len());
    Ok((written, updated))
  }
  /// Writes a byte array to an inode
  fn read_from_inode(
    &mut self,
    inode: &INode,
    dst: &mut [u8],
    offset: u32,
  ) -> Result<usize, ReadFromINodeErr> {
    let start_block = offset / (B::BLOCK_SIZE as u32);
    let end_byte = offset + dst.len() as u32;
    let end_block = (end_byte as usize + B::BLOCK_SIZE - 1) / (B::BLOCK_SIZE);
    if end_block > 8 {
      // Reading past the end of the file
      return Err(ReadFromINodeErr::ReadPastEnd);
    }
    let curr_blocks = (inode.size as usize + B::BLOCK_SIZE - 1) / B::BLOCK_SIZE;
    if curr_blocks < end_block {
      return Err(ReadFromINodeErr::ReadPastEnd);
    }
    let mut read = 0;
    let mut buf = [0; B::BLOCK_SIZE];
    for i in start_block..end_block.min(curr_blocks) as u32 {
      let db = inode.data_blocks[i as usize] as usize;
      let amt_read = self
        .gbi
        .read(self.data_md, db, &mut buf)
        .map_err(|_| ReadFromINodeErr::ReadErr)?;
      assert_eq!(amt_read, B::BLOCK_SIZE);
      let start = (read + offset as usize) % B::BLOCK_SIZE;
      let end = B::BLOCK_SIZE.min(start + dst.len() - read as usize);
      let read_buf = &buf[start..end];
      debug_assert_ne!(read_buf.len(), 0);
      dst[read..read + read_buf.len()].copy_from_slice(read_buf);
      read += read_buf.len();
    }
    assert_eq!(read, dst.len());
    Ok(read)
  }

  // A bunch of convenience functions which are implemented in terms of the above functions

  /// Convenience function for getting a directory from a file descriptor which is expected to
  /// be a file.
  pub fn as_directory(&mut self, fd: FileDescriptor) -> Result<Directory, AsDirErr> {
    let mut buf = [0; core::mem::size_of::<Directory>()];
    self.seek(fd, SeekFrom::Start(0))?;
    self.read(fd, &mut buf)?;
    Ok(unsafe { core::mem::transmute(buf) })
  }

  pub fn is_directory(&mut self, fd: FileDescriptor) -> Result<bool, IsDirErr> {
    let fde = self.file_descs[fd.0 as usize];
    let inode = self.load_inode(fde.inode as usize)?;
    Ok(matches!(inode.kind, INodeKind::Directory))
  }

  /// Function to convert an open file to a directory.
  pub fn modify_kind(&mut self, fd: FileDescriptor, kind: INodeKind) -> Result<(), ModifyKindErr> {
    let fdi = fd.0 as usize;
    let inode_num = self.file_descs.get(fdi).ok_or(FileDescOOB::OOB)?.inode as usize;
    let mut inode = self.load_inode(inode_num)?;
    match (inode.kind, kind) {
      (a, b) if a == b => return Ok(()),
      // Always fine
      (_, INodeKind::File) => {},
      (_, INodeKind::Contiguous) => todo!(),
      (_, INodeKind::Directory) =>
        if inode.size != core::mem::size_of::<Directory>() as u32 {
          return Err(ModifyKindErr::DirSizeMismatch);
        },
    }
    inode.kind = kind;
    self.save_inode(&inode, inode_num)?;
    Ok(())
  }

  /// Removes a directory from a given reference path with the given name
  pub fn rmdir(&mut self, fd: FileDescriptor, path: &[&str]) -> Result<(), RmdirErr> {
    let (fd, (curr_dir_inode_num, curr_dir_inode, curr_dir)) =
      self.open_with_dir(fd, path, FileMode::MustExist)?;
    if !self.is_directory(fd)? {
      self.close(fd)?;
      return Err(RmdirErr::NotDirectory);
    }

    let dir = self.as_directory(fd)?;
    if !dir.is_empty() {
      self.close(fd)?;
      return Err(RmdirErr::NotEmpty);
    }

    self.unlink_from_dir(
      fd,
      curr_dir_inode_num,
      curr_dir_inode,
      curr_dir,
      path.last().unwrap(),
    )?;
    Ok(())
  }

  /// Convenience function to make a directory inside of another directory
  pub fn mkdir(&mut self, fd: FileDescriptor, name: &str) -> Result<FileDescriptor, MkdirErr> {
    let new_fd = self.open(fd, &[name], FileMode::New)?;
    let fde = self.file_descs[fd.0 as usize];
    let own_fde = self.file_descs[new_fd.0 as usize];
    let new_dir = Directory::new(own_fde.inode, fde.inode);
    self.write(new_fd, new_dir.ser())?;
    self.modify_kind(new_fd, INodeKind::Directory)?;
    Ok(new_fd)
  }

  /// Returns stat about the file system
  pub fn fs_stats(&self) -> FsStats {
    FsStats {
      num_open_fds: self.file_descs.iter().filter(|fd| fd.open_refs > 0).count() as u32,
      num_open_inodes: self.open_counts.iter().filter(|&&v| v > 0).count() as u32,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsStats {
  num_open_fds: u32,
  num_open_inodes: u32,
}

macro_rules! define_error {
  ($name: ident: $( $own: ident ),* [] $( $nested_name: ident ( $nested: ty )),*) => {
    #[derive(Debug, Clone, Copy)]
    pub enum $name {
      $( $own, )*
      $( $nested_name($nested), )*
    }
    $(
      impl From<$nested> for $name {
        fn from(v: $nested) -> Self {
          Self::$nested_name(v)
        }
      }
    )*
  }
}

define_error!(FileDescOOB: OOB []);
define_error!(PersistAllocsErr: WriteErr []);
define_error!(LoadINodeErr: RWErr []);
define_error!(SaveINodeErr: WriteFailed, ReadFailed []);
define_error!(CacheINodeErr: [] SaveINodeErr(SaveINodeErr));

define_error!(ReadFromINodeErr: ReadPastEnd, ReadErr []);
define_error!(WriteToINodeErr: NotEnoughSpace, NoDataBlocks, RWErr []);
define_error!(
  ReadErr:
  NotReadable
  []
  ReadFromINodeErr(ReadFromINodeErr),
  LoadINodeErr(LoadINodeErr),
  FileDescOOB(FileDescOOB)
);
define_error!(
  OpenErr:
  PathDoesNotExist,
  NewButFileExists,
  MustProvideFileName,
  ModeCannotCreateFile,
  NotInsideDirectory,
  NoFreeFDs,
  FileOpenTooMuch
  []
  LoadINodeErr(LoadINodeErr),
  PersistAllocsErr(PersistAllocsErr),
  ReadFromINodeErr(ReadFromINodeErr),
  SaveINodeErr(SaveINodeErr),
  FileDescOOB(FileDescOOB),
  AllocINodeErr(AllocINodeErr),
  WriteToINodeErr(WriteToINodeErr)
);
define_error!(
  CloseErr:
  NotOpen
  []
  LoadINodeErr(LoadINodeErr),
  SaveINodeErr(SaveINodeErr),
  PersistAllocsErr(PersistAllocsErr)
);

define_error!(
  UnlinkErr:
  []
  OpenErr(OpenErr),
  SaveINodeErr(SaveINodeErr),
  LoadINodeErr(LoadINodeErr),
  CloseErr(CloseErr),
  WriteToINodeErr(WriteToINodeErr)
);
define_error!(
  SeekErr: BeforeStart [] LoadINodeErr(LoadINodeErr), FileDescOOB(FileDescOOB)
);
define_error!(StatErr: [] LoadINodeErr(LoadINodeErr), FileDescOOB(FileDescOOB));

define_error!(
  WriteErr:
  ReadOnly [] LoadINodeErr(LoadINodeErr),
  WriteToINodeErr(WriteToINodeErr),
  FileDescOOB(FileDescOOB),
  CacheINodeErr(CacheINodeErr)
);
define_error!(FlushErr: [] SaveINodeErr(SaveINodeErr));

define_error!(MkdirErr: [] OpenErr(OpenErr), WriteErr(WriteErr), ModifyKindErr(ModifyKindErr));
define_error!(
  RmdirErr:
  NotDirectory,
  NotEmpty
  []
  OpenErr(OpenErr),
  WriteErr(WriteErr),
  UnlinkErr(UnlinkErr),
  CloseErr(CloseErr),
  IsDirErr(IsDirErr),
  AsDirErr(AsDirErr)
);
define_error!(
  IsDirErr: [] LoadINodeErr(LoadINodeErr)
);
define_error!(
  ModifyKindErr:
  DirSizeMismatch
  []
  LoadINodeErr(LoadINodeErr),
  SaveINodeErr(SaveINodeErr),
  FileDescOOB(FileDescOOB)
);
define_error!(
  AsDirErr: [] SeekErr(SeekErr), ReadErr(ReadErr)
);
define_error!(
  AllocINodeErr: NoneFree [] PersistAllocsErr(PersistAllocsErr), SaveINodeErr(SaveINodeErr)
);
