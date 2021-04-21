pub const fn nearest_div_8(v: usize) -> usize { (v / 8) + if v % 8 == 0 { 0 } else { 1 } }

#[derive(Debug, PartialEq, Eq)]
pub struct BitArray<const N: usize>
where
  [(); nearest_div_8(N)]: , {
  pub(crate) items: [u8; nearest_div_8(N)],
}

impl<const N: usize> BitArray<N>
where
  [(); nearest_div_8(N)]: ,
{
  pub const fn new(full: bool) -> Self {
    let token = if full { 0xff } else { 0 };
    BitArray {
      items: [token; nearest_div_8(N)],
    }
  }
  /// Sets bit `i` in this bit array.
  pub const fn set(&mut self, i: usize) { self.items[i / 8] |= 1 << (i % 8); }
  /// Unsets bit `i` in this bit array.
  pub const fn unset(&mut self, i: usize) { self.items[i / 8] &= !(1 << (i % 8)); }
  /// Gets bit `i` in this bit array, where 1 => true, 0 => false.
  pub const fn get(&self, i: usize) -> bool { ((self.items[i / 8] >> (i % 8)) & 1) == 1 }
  /// Iterates through this bit array trying to find a free block, returning none if there are
  /// none.
  pub fn find_free(&self) -> Option<usize> {
    if N % 8 == 0 {
      return self
        .items
        .iter()
        .enumerate()
        .find(|(_, &v)| v != 0xff)
        .map(|(i, v)| {
          // TODO is this leading or trailing ones?
          i * 8 + v.trailing_ones() as usize
        });
    }
    self
      .items
      .iter()
      .take(N - 1)
      .enumerate()
      .find(|(_, &v)| v != 0xff)
      .map(|(i, v)| {
        // TODO is this leading or trailing ones?
        i * 8 + v.trailing_ones() as usize
      })
      .or_else(|| {
        // need to handle last item which may not be full byte
        let rem = N % 8;
        let mask = (0xFF << rem) >> rem;
        let last = self.items.last().unwrap() & mask;
        if last == mask {
          return None;
        }
        Some(8 * (N - 1) + last.trailing_ones() as usize)
      })
  }

  #[inline]
  pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
    // TODO make more efficient?
    (0..N).map(move |i| self.get(i))
  }
  pub fn num_free(&self) -> u32 {
    if N % 8 == 0 {
      return self.items.iter().map(|v| v.count_zeros()).sum::<u32>();
    }
    self
      .items
      .iter()
      .take(N - 1)
      .map(|v| v.count_zeros())
      .sum::<u32>()
      + self.items.last().unwrap().count_zeros().min((N % 8) as u32)
  }
}

// TODO write some tests for this to ensure it works
