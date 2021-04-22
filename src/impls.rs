use crate::{block_interface::BlockDevice, virtio::VirtIOBlk};

impl BlockDevice for VirtIOBlk<'_> {
  const NUM_BLOCKS: usize = 1000;
  const BLOCK_SIZE: usize = 512;
  fn read(&mut self, start_sector_num: u32, dst: &mut [u8]) -> Result<usize, ()> {
    let mut arr_chunk = dst.array_chunks_mut::<512>();
    let mut i = 0;
    for chunk in arr_chunk.by_ref() {
      self.read(start_sector_num as u64 + i, chunk);
      i += 1;
    }
    let rem = arr_chunk.into_remainder();
    if !rem.is_empty() {
      let mut buf = [0u8; 512];
      self.read(start_sector_num as u64 + i, &mut buf);
      rem.copy_from_slice(&buf[..rem.len()]);
    }
    Ok(dst.len())
  }
  fn write(&mut self, start_sector_num: u32, src: &[u8]) -> Result<usize, ()> {
    let mut arr_chunk = src.array_chunks::<512>();
    let mut i = 0;
    for chunk in arr_chunk.by_ref() {
      self.write(start_sector_num as u64 + i, chunk);
      i += 1;
    }
    let rem = arr_chunk.remainder();
    if !rem.is_empty() {
      let mut buf = [0u8; 512];
      // read in what's already there, overwrite the beginning and overwrite the end.
      self.read(start_sector_num as u64 + i, &mut buf);
      buf[..rem.len()].copy_from_slice(rem);
      self.write(start_sector_num as u64 + i, &mut buf);
    }
    Ok(src.len())
  }
}
