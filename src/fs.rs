use crate::{
  bit_array::{nearest_div_8, BitArray},
  block_interface::{
    AllMetadata, BlockDevice, GlobalBlockInterface, Metadata, MetadataHandle, Owner,
    FIRST_FREE_BLOCK, OWN_BLOCKS,
  },
  default_ser_impl,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FileDescriptor(u32);

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FileMode {
  R,
  W,
  RW,
}

impl Default for FileMode {
  fn default() -> Self { Self::R }
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
  /*
  /// Returns all the byte array entries of this directory
  #[inline]
  pub(crate) fn entries(&self) -> impl Iterator<Item = (&[u8], u32)> {
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
  */
  /// Inserts an entry into this directory
  // TODO convert this into a result type
  pub fn insert(&mut self, name: &str, to: u32) -> Result<(), ()> {
    if self.num_added as usize >= NUM_ENTRIES {
      // TODO directory out of space
      return Err(());
    }
    if self.contains(name) {
      // TODO already contains file with that name
      return Err(());
    }
    //  TODO check other names if they match
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
}

impl From<u8> for INodeKind {
  fn from(v: u8) -> Self {
    match v {
      0 => INodeKind::File,
      1 => INodeKind::Directory,
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

/// The number of inodes this file system supports.
pub const NUM_INODE: usize = 1024;
/// The number of data blocks in this system.
pub const NUM_DATA: usize = 2048;
// TODO for some reason I can't just plug it in below so I need to hard code this

/// A singleton file system type.
pub struct FileSystem<B: 'static + BlockDevice>
where
  B: BlockDevice,
  [(); nearest_div_8(B::NUM_BLOCKS)]: ,
  [(); B::BLOCK_SIZE]: , {
  superblock: MetadataHandle,
  file_descs: [FileDescEntry; 256],
  inode_md: MetadataHandle,
  data_md: MetadataHandle,
  inode_alloc_map: BitArray<1024>,
  data_alloc_map: BitArray<2048>,
  gbi: &'static mut GlobalBlockInterface<B>,
}

impl<B> FileSystem<B>
where
  B: BlockDevice,
  [(); nearest_div_8(B::NUM_BLOCKS)]: ,
  [(); B::BLOCK_SIZE]: ,
  [(); 2 * B::BLOCK_SIZE]: ,
  [(); OWN_BLOCKS * B::BLOCK_SIZE]: ,
{
  pub fn new(gbi: &'static mut GlobalBlockInterface<B>) -> Self {
    let sb_mh = gbi
      .metadatas_for(Owner::LibFS)
      .find_map(|(mh, amd)| matches!(amd, AllMetadata::Superblock(_)).then(|| mh));
    if let Some(sb_mh) = sb_mh {
      // File system previously existed, reinitialize
      let mut mhs = gbi
        .metadatas_for(Owner::LibFS)
        .filter(|(_, amd)| matches!(amd, AllMetadata::RangeMetadata(_)))
        .map(|v| v.0);
      let inode_mh = mhs.next().expect("Got inode metadata handle");
      let data_mh = mhs.next().expect("Got inode metadata handle");
      assert!(mhs.next().is_none());
      drop(mhs);
      let mut out = Self {
        superblock: sb_mh,
        inode_md: inode_mh,
        data_md: data_mh,
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
  pub fn mk(gbi: &'static mut GlobalBlockInterface<B>) -> Self {
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
      inode_alloc_map: BitArray::new(false),
      data_alloc_map: BitArray::new(false),
      file_descs: [FileDescEntry::default(); 256],
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
    Ok(FileDescriptor(i as u32))
  }
  /// Seeks inside of a file
  pub fn seek(&mut self, FileDescriptor(fdi): FileDescriptor, s: SeekFrom) -> Result<(), ()> {
    let fdi = fdi as usize;
    let fd = self.file_descs.get_mut(fdi).ok_or(())?;
    match s {
      SeekFrom::Start(v) => fd.offset = v,
      SeekFrom::End(v) => {
        let inode_num = fd.inode;
        drop(fd);
        let inode = self.load_inode(inode_num as usize)?;
        let dst = inode.size as i32 + v;
        if dst < 0 {
          return Err(());
        }
        self.file_descs[fdi].offset = dst as u32;
      },
      SeekFrom::Current(v) => {
        let dst = fd.offset as i32 + v;
        if dst < 0 {
          return Err(());
        }
        fd.offset = dst as u32;
      },
    }
    Ok(())
  }
  ///
  pub fn read(&mut self, FileDescriptor(fdi): FileDescriptor, dst: &mut [u8]) -> Result<usize, ()> {
    let fdi = fdi as usize;
    let fd = self.file_descs.get(fdi).ok_or(())?;
    let offset = fd.offset;
    if fd.mode == FileMode::W {
      return Err(());
    }
    let inode = self.load_inode(fd.inode as usize)?;
    if inode.kind == INodeKind::Directory {
      assert_eq!(dst.len(), core::mem::size_of::<Directory>());
    }
    let read = self.read_from_inode(&inode, dst, offset)?;
    self.file_descs[fdi].offset += read as u32;
    Ok(read)
  }
  pub fn write(&mut self, FileDescriptor(fdi): FileDescriptor, src: &[u8]) -> Result<usize, ()> {
    let fdi = fdi as usize;
    let fd = self.file_descs.get(fdi).ok_or(())?.clone();
    if fd.mode == FileMode::R {
      return Err(());
    }
    let mut inode = self.load_inode(fd.inode as usize)?;
    if inode.kind == INodeKind::Directory {
      assert_eq!(src.len(), core::mem::size_of::<Directory>());
    }
    let written = self.write_to_inode(&mut inode, src, fd.offset)?;
    self.save_inode(&inode, fd.inode as usize)?;
    self.file_descs[fdi].offset += written as u32;
    Ok(written)
  }
  pub fn stat(&self, FileDescriptor(fdi): FileDescriptor) -> Result<FileStat, ()> {
    let fdi = fdi as usize;
    let fd = self.file_descs.get(fdi).ok_or(())?;
    let inode = self.load_inode(fd.inode as usize)?;
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
  const fn inode_block_and_offset_and_wraps(i: usize) -> (usize, usize, bool) {
    // Should round down
    let bl = (i * core::mem::size_of::<INode>()) / B::BLOCK_SIZE;
    let next_bl = ((i + 1) * core::mem::size_of::<INode>()) / B::BLOCK_SIZE;
    let wraps = bl != next_bl;
    let offset = (i * core::mem::size_of::<INode>()) % B::BLOCK_SIZE;
    (bl, offset, wraps)
  }
  fn load_inode(&self, i: usize) -> Result<INode, ()> {
    assert!(
      self.inode_alloc_map.get(i),
      "Unallocated inode is being loaded"
    );
    let (block, offset, overlaps_end) = Self::inode_block_and_offset_and_wraps(i);
    if !overlaps_end {
      let mut buf = [0; B::BLOCK_SIZE];
      self.gbi.read(self.inode_md, block, &mut buf)?;
      Ok(INode::from_slice(
        &buf[offset..offset + core::mem::size_of::<INode>()],
      ))
    } else {
      let mut buf = [0u8; 2 * B::BLOCK_SIZE];
      self
        .gbi
        .read(self.inode_md, block, &mut buf[..B::BLOCK_SIZE])?;
      self
        .gbi
        .read(self.inode_md, block + 1, &mut buf[B::BLOCK_SIZE..])?;
      Ok(INode::from_slice(
        &buf[offset..offset + core::mem::size_of::<INode>()],
      ))
    }
  }
  #[must_use]
  fn save_inode(&self, inode: &INode, i: usize) -> Result<(), ()> {
    assert!(
      self.inode_alloc_map.get(i as usize),
      "Unallocated inode is being saved"
    );
    let (block, offset, wraps) = Self::inode_block_and_offset_and_wraps(i);
    if !wraps {
      let mut buf = [0u8; B::BLOCK_SIZE];
      self.gbi.read(self.inode_md, block, &mut buf)?;
      inode.to_slice(&mut buf[offset..offset + core::mem::size_of::<INode>()]);
      self.gbi.write(self.inode_md, block, &buf)?;
    } else {
      let mut buf = [0u8; 2 * B::BLOCK_SIZE];
      self
        .gbi
        .read(self.inode_md, block, &mut buf[..B::BLOCK_SIZE])?;
      self
        .gbi
        .read(self.inode_md, block + 1, &mut buf[B::BLOCK_SIZE..])?;
      inode.to_slice(&mut buf[offset..offset + core::mem::size_of::<INode>()]);
      self
        .gbi
        .write(self.inode_md, block, &buf[..B::BLOCK_SIZE])?;
      self
        .gbi
        .write(self.inode_md, block + 1, &buf[B::BLOCK_SIZE..])?;
    }
    Ok(())
  }
  /// Given an inode, saves it and returns the inode number it was given
  fn alloc_inode(&mut self, inode: &INode) -> Result<u32, ()> {
    let inode_num = self.inode_alloc_map.find_free().ok_or(())? as u32;
    self.inode_alloc_map.set(inode_num as usize);
    self.save_inode(inode, inode_num as usize)?;
    self.persist_allocs()?;
    Ok(inode_num)
  }
  /*
  /// Frees an inode from the inode alloc map
  fn free_inode(&mut self, i: usize) -> Result<(), ()> {
    assert!(
      self.inode_alloc_map.get(i),
      "Trying to deallocate unallocated inode {}",
      i
    );
    self.inode_alloc_map.unset(i);
    self.persist_allocs()?;
    Ok(())
  }
  */
  /// Opens a given path relative to dir.
  pub fn open(
    &mut self,
    // Inside of which directory?
    FileDescriptor(fdi): FileDescriptor,
    path: &[&str],
    mode: FileMode,
  ) -> Result<FileDescriptor, ()> {
    if path.is_empty() {
      // Must have at least one entry in the path
      return Err(());
    }
    let mut curr_dir_inode_num = self.file_descs.get(fdi as usize).ok_or(())?.inode;
    let mut curr_dir_inode = self.load_inode(curr_dir_inode_num as usize)?;
    if curr_dir_inode.kind != INodeKind::Directory {
      // println!("{:?} {:?}", curr_dir_inode, curr_dir_inode_num);
      // Can't create a file inside of something which isn't a directory
      return Err(());
    }
    let mut buf = [0u8; core::mem::size_of::<Directory>()];
    self.read_from_inode(&curr_dir_inode, &mut buf, 0)?;
    let mut curr_dir: Directory = unsafe { core::mem::transmute(buf) };
    for subdir in path.iter().take(path.len() - 1) {
      curr_dir_inode_num = curr_dir.inode_of(subdir).ok_or(())?;
      curr_dir_inode = self.load_inode(curr_dir_inode_num as usize)?;
      if curr_dir_inode.kind != INodeKind::Directory {
        return Err(());
      }
      let mut buf: [u8; core::mem::size_of::<Directory>()] =
        unsafe { core::mem::transmute(curr_dir) };
      self.read_from_inode(&curr_dir_inode, &mut buf, 0)?;
      curr_dir = unsafe { core::mem::transmute(buf) };
    }
    let last_entry = path.last().unwrap();
    let inode_num = if let Some(inode_num) = curr_dir.inode_of(last_entry) {
      inode_num
    } else if mode != FileMode::R {
      let inode = INode::new(INodeKind::File);
      let inode_num = self.alloc_inode(&inode)?;
      curr_dir
        .insert(last_entry, inode_num)
        .expect("Failed to insert name into directory");
      let buf: [u8; core::mem::size_of::<Directory>()] = unsafe { core::mem::transmute(curr_dir) };
      self.write_to_inode(&mut curr_dir_inode, &buf, 0)?;
      self.save_inode(&curr_dir_inode, curr_dir_inode_num as usize)?;
      inode_num
    } else
    /* mode == FileMode::R */
    {
      return Err(());
    };
    let (i, fd) = self
      .file_descs
      .iter_mut()
      .enumerate()
      .find(|(_, fd)| fd.open_refs == 0)
      .ok_or(())?;
    fd.inode = inode_num;
    fd.offset = 0;
    fd.open_refs += 1;
    fd.mode = mode;
    Ok(FileDescriptor(i as u32))
  }
  /// Unlinks a file in the directory given by the file descriptor, following the path.
  pub fn unlink(&mut self, FileDescriptor(fdi): FileDescriptor, path: &[&str]) -> Result<(), ()> {
    if path.is_empty() {
      return Err(());
    }
    let mut curr_dir_inode_num = self.file_descs.get(fdi as usize).ok_or(())?.inode;
    let mut curr_dir_inode = self.load_inode(curr_dir_inode_num as usize)?;
    if curr_dir_inode.kind != INodeKind::Directory {
      // Can't create a file inside of something which isn't a directory
      return Err(());
    }
    let mut buf = [0u8; core::mem::size_of::<Directory>()];
    self.read_from_inode(&curr_dir_inode, &mut buf, 0)?;
    let mut curr_dir: Directory = unsafe { core::mem::transmute(buf) };
    for subdir in path.iter().take(path.len() - 1) {
      curr_dir_inode_num = curr_dir.inode_of(subdir).ok_or(())?;
      curr_dir_inode = self.load_inode(curr_dir_inode_num as usize)?;
      let mut buf: [u8; core::mem::size_of::<Directory>()] =
        unsafe { core::mem::transmute(curr_dir) };
      self.read_from_inode(&curr_dir_inode, &mut buf, 0)?;
      curr_dir = unsafe { core::mem::transmute(buf) };
    }
    let last_entry = path.last().unwrap();
    let inode_num = curr_dir.inode_of(last_entry).ok_or(())?;
    let mut inode = self.load_inode(inode_num as usize)?;
    if inode.kind == INodeKind::Directory {
      // Do not permit removing directories for now
      // TODO either check if the directory is empty or allow for -r arguments to this function
      return Err(());
    }
    inode.refs -= 1;
    self.save_inode(&inode, inode_num as usize)?;
    curr_dir.remove(curr_dir.index_of(last_entry).unwrap());
    let buf: [u8; core::mem::size_of::<Directory>()] = unsafe { core::mem::transmute(curr_dir) };
    self.write_to_inode(&mut curr_dir_inode, &buf, 0)?;
    self.save_inode(&curr_dir_inode, curr_dir_inode_num as usize)?;
    Ok(())
  }
  pub fn close(&mut self, FileDescriptor(i): FileDescriptor) -> Result<(), ()> {
    let i = i as usize;
    let open_file = &mut self.file_descs[i];
    if open_file.open_refs == 0 {
      return Err(());
    }
    open_file.open_refs -= 1;
    // If inode is no longer referenced here delete it
    if open_file.open_refs == 0 {
      let inode_num = open_file.inode as usize;
      let mut inode = self.load_inode(inode_num)?;
      if inode.refs > 0 {
        return Ok(());
      }
      for d in inode.data_blocks.iter_mut() {
        self.data_alloc_map.unset(*d as usize);
        *d = 0;
      }
      self.save_inode(&inode, inode_num)?;
      self.inode_alloc_map.unset(inode_num);
      self.persist_allocs()?;
    }
    Ok(())
  }
  /// Saves the allocation maps to disk
  fn persist_allocs(&self) -> Result<(), ()> {
    let mut buf = [0u8; { (NUM_INODE + NUM_DATA) / 8 }];
    assert!(buf.len() < B::BLOCK_SIZE);
    buf[..self.inode_alloc_map.items.len()].copy_from_slice(&self.inode_alloc_map.items);
    buf[self.inode_alloc_map.items.len()..].copy_from_slice(&self.data_alloc_map.items);
    self.gbi.write(self.superblock, 0, &buf)?;
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
  fn write_to_inode(&mut self, inode: &mut INode, data: &[u8], offset: u32) -> Result<usize, ()> {
    let start_block = offset / (B::BLOCK_SIZE as u32);
    let end_byte = offset + data.len() as u32;
    let end_block = (end_byte as usize + B::BLOCK_SIZE - 1) / (B::BLOCK_SIZE);
    if end_block > 8 {
      // Not enough space in these files to write that much.
      // TODO lift this when I figure out a way to allocate more space
      return Err(());
    }
    let curr_blocks = (inode.size as usize + B::BLOCK_SIZE - 1) / B::BLOCK_SIZE;
    // TODO check the number of free data blocks before and abort early if so;
    if curr_blocks < end_block {
      for i in curr_blocks..end_block {
        let free_db = self.data_alloc_map.find_free().ok_or(())?;
        inode.data_blocks[i] = free_db as u16;
        self.data_alloc_map.set(free_db);
      }
    }
    inode.size = inode.size.max(end_byte);
    let mut written = 0;
    let mut buf = [0; B::BLOCK_SIZE];
    for i in start_block..end_block as u32 {
      let db = inode.data_blocks[i as usize] as usize;
      let read = self.gbi.read(self.data_md, db, &mut buf)?;
      assert_eq!(read, B::BLOCK_SIZE);
      let start = (written + offset as usize) % B::BLOCK_SIZE;
      let end = B::BLOCK_SIZE.min(start + data.len() - written as usize);
      let write_buf = &mut buf[start..end];
      debug_assert_ne!(write_buf.len(), 0);
      write_buf.copy_from_slice(&data[written..written + write_buf.len()]);
      written += write_buf.len();
      self.gbi.write(self.data_md, db, &buf)?;
    }
    assert_eq!(written, data.len());
    Ok(written)
  }
  /// Writes a byte array to an inode
  fn read_from_inode(&mut self, inode: &INode, dst: &mut [u8], offset: u32) -> Result<usize, ()> {
    let start_block = offset / (B::BLOCK_SIZE as u32);
    let end_byte = offset + dst.len() as u32;
    let end_block = (end_byte as usize + B::BLOCK_SIZE - 1) / (B::BLOCK_SIZE);
    if end_block > 8 {
      // Reading past the end of the file
      return Err(());
    }
    let curr_blocks = (inode.size as usize + B::BLOCK_SIZE - 1) / B::BLOCK_SIZE;
    if curr_blocks < end_block {
      return Err(());
    }
    let mut read = 0;
    let mut buf = [0; B::BLOCK_SIZE];
    for i in start_block..end_block.min(curr_blocks) as u32 {
      let db = inode.data_blocks[i as usize] as usize;
      assert_eq!(self.gbi.read(self.data_md, db, &mut buf)?, B::BLOCK_SIZE);
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
  pub fn as_directory(&mut self, fd: FileDescriptor) -> Result<Directory, ()> {
    let mut buf = [0; core::mem::size_of::<Directory>()];
    self.seek(fd, SeekFrom::Start(0))?;
    self.read(fd, &mut buf)?;
    Ok(unsafe { core::mem::transmute(buf) })
  }

  /// Function to convert an open file to a directory.
  pub fn modify_kind(&mut self, fd: FileDescriptor, kind: INodeKind) -> Result<(), ()> {
    let fdi = fd.0 as usize;
    let fde = self.file_descs.get(fdi).ok_or(())?;
    let mut inode = self.load_inode(fde.inode as usize)?;
    match (inode.kind, kind) {
      (a, b) if a == b => return Ok(()),
      // Always fine
      (_, INodeKind::File) => {},
      (_, INodeKind::Directory) =>
        if inode.size != core::mem::size_of::<Directory>() as u32 {
          return Err(());
        },
    }
    inode.kind = kind;
    self.save_inode(&inode, fde.inode as usize)?;
    Ok(())
  }

  /// Convenience function to make a directory inside of another directory
  pub fn mkdir(&mut self, fd: FileDescriptor, name: &str) -> Result<FileDescriptor, ()> {
    let new_fd = self.open(fd, &[name], FileMode::RW)?;
    let fde = self.file_descs[fd.0 as usize];
    let own_fde = self.file_descs[new_fd.0 as usize];
    let new_dir = Directory::new(own_fde.inode, fde.inode);
    self.write(new_fd, new_dir.ser())?;
    self.modify_kind(new_fd, INodeKind::Directory)?;
    Ok(new_fd)
  }
}
