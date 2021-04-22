#![no_main]
#![no_std]
#![feature(
  asm,
  global_asm,
  maybe_uninit_extra,
  const_generics,
  maybe_uninit_uninit_array,
  const_evaluatable_checked,
  array_methods,
  const_mut_refs,
  const_fn_transmute,
  const_fn,
  array_chunks,
  generic_associated_types
)]
#![allow(incomplete_features)]

pub mod device_tree;
pub mod uart;
pub mod utils;
pub mod virtio;

pub mod bit_array;
pub mod block_interface;
pub mod fs;

pub mod impls;

use block_interface::GlobalBlockInterface;
use core::ptr::read_volatile;
use virtio::{VirtIOBlkConfig, VirtIODevice, VirtIORegs};

#[cfg(target_arch = "aarch64")]
global_asm!(include_str!("boot.S"));

use core::{fmt::Write, panic::PanicInfo, str::from_utf8};

fn null_terminated_str(bytes: &[u8]) -> &[u8] {
  if bytes[bytes.len() - 1] == 0 {
    &bytes[..bytes.len() - 1]
  } else {
    bytes
  }
}

fn regs_to_usize(regs: &[u8], cell_size: usize) -> (usize, &[u8]) {
  let mut result = 0;
  let (work, rest) = regs.split_at(cell_size * 4);
  for chunk in work.chunks(4) {
    let mut c = [0; 4];
    c.copy_from_slice(chunk);
    result = result << 32 | (u32::from_be_bytes(c) as usize);
  }
  (result, rest)
}

#[no_mangle]
pub extern "C" fn kernel_main(dtb: &device_tree::DeviceTree) {
  let mut uart = None;
  if let Some(root) = dtb.root() {
    let size_cell = root
      .prop_by_name("#size-cells")
      .map(|sc| {
        let mut buf = [0; 4];
        buf.copy_from_slice(sc.value);
        u32::from_be_bytes(buf) as usize
      })
      .unwrap_or(2);
    let address_cell = root
      .prop_by_name("#address-cells")
      .map(|sc| {
        let mut buf = [0; 4];
        buf.copy_from_slice(sc.value);
        u32::from_be_bytes(buf) as usize
      })
      .unwrap_or(2);

    if let Some(chosen) = root.child_by_name("chosen") {
      chosen
        .prop_by_name("stdout-path")
        .map(|stdout_path| null_terminated_str(stdout_path.value))
        .filter(|stdout_path| stdout_path == b"/pl011@9000000")
        .map(|stdout_path| {
          root.child_by_path(stdout_path).map(|stdout| {
            if let Some(reg) = stdout.prop_by_name("reg") {
              let (addr, rest) = regs_to_usize(reg.value, address_cell);
              let (size, _) = regs_to_usize(rest, size_cell);
              if size == 0x1000 {
                uart = Some(unsafe { uart::UART::new(addr as _) });
              }
            }
          });
        });
    }
    let mut uart = if let Some(uart) = uart {
      uart
    } else {
      return;
    };

    let _ = write!(uart, "We booted!\n");

    let mut virtio_blk = None;
    let mut blk_desc = [virtio::VirtQDesc::empty(); 128];
    let mut blk_avail = virtio::VirtQAvailable::empty();
    let mut blk_used = virtio::VirtQUsed::empty();

    let mut virtio_entropy = None;
    let mut entropy_desc = [virtio::VirtQDesc::empty(); 128];
    let mut entropy_avail = virtio::VirtQAvailable::empty();
    let mut entropy_used = virtio::VirtQUsed::empty();

    for child in root.children_by_prop("compatible", |prop| prop.value == b"virtio,mmio\0") {
      if let Some(reg) = child.prop_by_name("reg") {
        let (addr, _rest) = regs_to_usize(reg.value, address_cell);
        if let Some(virtio) = unsafe { VirtIORegs::new(addr as *mut VirtIORegs) } {
          match virtio.device_id() {
            virtio::DeviceId::Blk => {
              virtio_blk =
                virtio::VirtIOBlk::init(virtio, &mut blk_desc, &mut blk_avail, &mut blk_used);
            },
            virtio::DeviceId::Entropy => {
              virtio_entropy = virtio::VirtIOEntropy::init(
                virtio,
                &mut entropy_desc,
                &mut entropy_avail,
                &mut entropy_used,
              );
            },
            _ => {},
          }
        }
      }
    }

    let virtio_blk_cfg: VirtIOBlkConfig = unsafe {
      read_volatile(&virtio_blk.as_ref().unwrap().regs.config.native() as *const u64 as *const _)
    };
    let _ = write!(uart, "Capacity {:?}\n", virtio_blk_cfg);

    // let gbi = GlobalBlockInterface::new(virtio_blk.unwrap());

    loop {
      let _ = write!(uart, "$> ");
      let mut buf = [0; 1024];
      let line = uart.read_line(&mut buf, true);
      let mut words = line.split(|c| *c == b' ');
      let word = if let Some(word) = words.next() {
        word
      } else {
        continue;
      };
      match word {
        b"rand" => {
          let mut data: [u8; 16] = [0; 16];
          virtio_entropy.as_mut().map(|v| v.read(&mut data));
          let _ = write!(uart, "Random: {:?}\n", &data);
        },
        b"writerand" => {
          let mut sector = words
            .next()
            .and_then(|sec| from_utf8(sec).ok())
            .and_then(|sec| sec.parse::<u64>().ok())
            .unwrap_or(0);
          let mut len = words
            .next()
            .and_then(|len| from_utf8(len).ok())
            .and_then(|len| len.parse::<usize>().ok())
            .unwrap_or(0);
          while len > 0 {
            let mut outdata: [u8; 512] = [0; 512];
            let curlen = core::cmp::min(512, len);
            {
              let curbuf = &mut outdata[..curlen];
              virtio_entropy.as_mut().map(|v| v.read(curbuf));
              for b in curbuf.iter_mut() {
                *b = ((*b as u32 * 100) / 272 + 32) as u8;
              }
            }
            virtio_blk.as_mut().map(|v| v.write(sector, &outdata));
            sector += 1;
            len -= curlen;
          }
        },
        b"read" => {
          let sector = words
            .next()
            .and_then(|sec| from_utf8(sec).ok())
            .and_then(|sec| sec.parse::<u64>().ok())
            .unwrap_or(0);
          let mut len = words
            .next()
            .and_then(|len| from_utf8(len).ok())
            .and_then(|len| len.parse::<usize>().ok())
            .unwrap_or(512);
          let mut data: [u8; 512] = [0; 512];
          loop {
            virtio_blk.as_mut().map(|v| v.read(sector, &mut data));
            if len > 512 {
              uart.write_bytes(&data);
              len -= 512;
            } else {
              uart.write_bytes(&data[..len]);
              uart.write_byte(b'\n');
              break;
            }
          }
        },
        b"write" => {
          let mut sector = words
            .next()
            .and_then(|sec| from_utf8(sec).ok())
            .and_then(|sec| sec.parse::<u64>().ok())
            .unwrap_or(0);
          let mut len = words
            .next()
            .and_then(|len| from_utf8(len).ok())
            .and_then(|len| len.parse::<usize>().ok())
            .unwrap_or(0);
          while len > 0 {
            let mut outdata: [u8; 512] = [0; 512];
            let curlen = core::cmp::min(512, len);
            {
              let curbuf = &mut outdata[..curlen];
              for b in curbuf.iter_mut() {
                *b = uart.read_byte();
                if *b == b'\r' {
                  *b = b'\n';
                }
                uart.write_byte(*b);
              }
            }
            virtio_blk.as_mut().map(|v| v.write(sector, &outdata));
            sector += 1;
            len -= curlen;
          }
        },
        b"exit" => break,
        _ => {
          let _ = write!(
            uart,
            "Unknown command \"{}\"\n",
            from_utf8(line).unwrap_or("unknown")
          );
        },
      }
    }
  }
}

#[panic_handler]
fn panic(panic_info: &PanicInfo<'_>) -> ! {
  let mut uart = unsafe { uart::UART::new(0x0900_0000 as _) };
  let _ = write!(uart, "Panic occurred: {}\n", panic_info);
  loop {}
}
