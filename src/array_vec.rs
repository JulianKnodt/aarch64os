use core::mem::MaybeUninit;

#[derive(Debug)]
pub struct ArrayVec<T, const N: usize, C = u8> {
  items: [MaybeUninit<T>; N],
  curr_len: C,
}

impl<T, const N: usize> ArrayVec<T, N, u8> {
  pub fn new() -> Self {
    assert!(N <= (u8::MAX as usize));
    Self {
      items: MaybeUninit::uninit_array(),
      curr_len: 0,
    }
  }
  /// pushes an element into this array vec, but returns it if the vec is full.
  pub fn push(&mut self, v: T) -> Option<T> {
    if self.curr_len as usize == N {
      return Some(v);
    }
    self.items[self.curr_len as usize] = MaybeUninit::new(v);
    self.curr_len += 1;
    None
  }
  pub fn as_slice(&self) -> &[T] {
    unsafe { MaybeUninit::slice_assume_init_ref(&self.items[..self.curr_len as usize]) }
  }
  pub fn as_mut_slice(&mut self) -> &mut [T] {
    unsafe { MaybeUninit::slice_assume_init_mut(&mut self.items[..self.curr_len as usize]) }
  }
  pub fn push_front(&mut self, v: T) -> Option<T> {
    if self.curr_len as usize == N {
      return Some(v);
    }
    self.items.rotate_right(1);
    self.items[0] = MaybeUninit::new(v);
    self.curr_len += 1;
    None
  }
  pub fn push_out_front(&mut self, v: T) -> Option<T> {
    let old = if self.curr_len as usize == N {
      self.pop()
    } else {
      None
    };
    self.items.rotate_right(1);
    self.items[0] = MaybeUninit::new(v);
    self.curr_len += 1;
    old
  }
  pub fn pop(&mut self) -> Option<T> {
    if self.curr_len == 0 {
      return None;
    }
    self.curr_len -= 1;
    let old = core::mem::replace(
      &mut self.items[self.curr_len as usize],
      MaybeUninit::zeroed(),
    );
    Some(unsafe { old.assume_init() })
  }
  pub fn remove_where(&mut self, pred: impl FnMut(&T) -> bool) -> Option<T> {
    let p = self.as_slice().iter().position(pred)?;
    self.items[p..self.curr_len as usize].rotate_left(1);
    self.pop()
  }
}
