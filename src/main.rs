#![no_main]
#![no_std]
#![feature(
  asm,
  global_asm,
  maybe_uninit_extra,
  const_generics,
  maybe_uninit_uninit_array,
  maybe_uninit_slice,
  const_evaluatable_checked,
  array_methods,
  const_mut_refs,
  const_fn_transmute,
  const_fn,
  array_chunks,
  const_generics_defaults,
  specialization,
  int_bits_const,
  generic_associated_types
)]
#![allow(incomplete_features)]

pub mod device_tree;
pub mod uart;
pub mod utils;
pub mod virtio;

pub mod array_vec;
pub mod bit_array;
pub mod block_interface;
pub mod fs;

pub mod impls;

use block_interface::GlobalBlockInterface;
use core::ptr::read_volatile;
use virtio::{VirtIOBlkConfig, VirtIODevice, VirtIORegs};

#[cfg(target_arch = "aarch64")]
global_asm!(include_str!("boot.S"));

use core::{
  fmt::Write,
  panic::PanicInfo,
  str::{from_utf8, FromStr},
};

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
          let stdout = root.child_by_path(stdout_path);
          if let Some(reg) = stdout.prop_by_name("reg") {
            let (addr, rest) = regs_to_usize(reg.value, address_cell);
            let (size, _) = regs_to_usize(rest, size_cell);
            if size == 0x1000 {
              uart = Some(unsafe { uart::UART::new(addr as _) });
            }
          }
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

    let mut virtio_entropy = virtio_entropy.unwrap();

    let virtio_blk_cfg: VirtIOBlkConfig = unsafe {
      read_volatile(&virtio_blk.as_ref().unwrap().regs.config.native() as *const u64 as *const _)
    };
    let _ = write!(uart, "Num. Sectors {:?}\n", virtio_blk_cfg.capacity);

    let mut gbi = GlobalBlockInterface::new(virtio_blk.unwrap());
    gbi.try_init().expect("Failed to init");
    let mut fs = fs::FileSystem::new(&mut gbi);

    let mut curr_dir = fs
      .root_dir(fs::FileMode::RW)
      .expect("Failed to get root directory");

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
        b"ls" => {
          let dir = fs.as_directory(curr_dir).unwrap();
          let entries = dir
            .entries()
            .map(|v| v.0)
            .map(|name| core::str::from_utf8(name).unwrap());
          for name in entries {
            let _ = write!(uart, "{}\n", name);
          }
        },
        b"open" => {
          let w = words.next().and_then(|w| core::str::from_utf8(w).ok());
          let file_name = if let Some(word) = w {
            word
          } else {
            let _ = writeln!(uart, "Usage: open <file_name> <kind=RW>");
            continue;
          };
          let mode = words
            .next()
            .and_then(|w| core::str::from_utf8(w).ok())
            .and_then(|w| fs::FileMode::from_str(w).ok())
            .unwrap_or(fs::FileMode::RW);
          let fd = match fs.open(curr_dir, &[file_name], mode) {
            Ok(fd) => fd,
            Err(err) => {
              let _ = writeln!(uart, "Open failed: {:?}", err);
              continue;
            },
          };
          let _ = writeln!(uart, "Opened file: {:?}", fd);
        },
        b"mkdir" => {
          let w = words.next().and_then(|w| core::str::from_utf8(w).ok());
          let dir_name = if let Some(word) = w {
            word
          } else {
            let _ = write!(uart, "Usage: mkdir <dir>");
            continue;
          };
          if let Err(e) = fs.mkdir(curr_dir, dir_name) {
            let _ = write!(uart, "mkdir failed: {:?}\n", e);
          }
        },
        b"rmdir" => {
          let dir_name = words.next().and_then(|w| core::str::from_utf8(w).ok());
          let dir_name = if let Some(dir_name) = dir_name {
            dir_name
          } else {
            let _ = writeln!(uart, "Usage: fwriterand <dir_name>");
            continue;
          };
          if let Err(err) = fs.rmdir(curr_dir, &[dir_name]) {
            let _ = writeln!(uart, "Failed to rmdir {}: {:?}", dir_name, err);
          }
        },
        b"cd" => {
          let w = words.next().and_then(|w| core::str::from_utf8(w).ok());
          let dir_name = if let Some(word) = w {
            word
          } else {
            curr_dir = fs.root_dir(fs::FileMode::RW).unwrap();
            continue;
          };
          let next_dir = match fs.open(curr_dir, &[dir_name], fs::FileMode::MustExist) {
            Ok(v) => v,
            Err(e) => {
              let _ = write!(uart, "cd failed: {:?}\n", e);
              continue;
            },
          };
          if let Err(err) = fs.close(curr_dir) {
            let _ = write!(uart, "Could not properly close old directory: {:?}", err);
          }
          curr_dir = next_dir;
        },
        b"rand" => {
          let mut data: [u8; 16] = [0; 16];
          virtio_entropy.read(&mut data);
          let _ = write!(uart, "Random: {:?}\n", &data);
        },
        b"fread" => {
          let fd = words
            .next()
            .and_then(|fd| from_utf8(fd).ok())
            .and_then(|fd| fd.parse::<u32>().ok());
          let fd = if let Some(fd) = fd {
            fs::FileDescriptor::from(fd)
          } else {
            let _ = writeln!(uart, "Usage: fread <file_descriptor> <len=512>");
            continue;
          };
          let mut len = words
            .next()
            .and_then(|len| from_utf8(len).ok())
            .and_then(|len| len.parse::<usize>().ok())
            .unwrap_or(512);
          let mut data = [0; 512];
          while len > 0 {
            let rem = len.min(512);
            match fs.read(fd, &mut data[..rem]) {
              Ok(read) => {
                len -= read;
                uart.write_bytes(&data[..rem]);
              },
              Err(e) => {
                let _ = writeln!(uart, "Failed to read from {:?}: {:?}", fd, e);
                break;
              },
            }
          }
          loop {
            //gbi.block_device.read(sector, &mut data);
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
        b"fwriterand" => {
          let fd = words
            .next()
            .and_then(|fd| from_utf8(fd).ok())
            .and_then(|fd| fd.parse::<u32>().ok());
          let fd = if let Some(fd) = fd {
            fs::FileDescriptor::from(fd)
          } else {
            let _ = writeln!(uart, "Usage: fwriterand <file_descriptor> <len=512>");
            continue;
          };

          let mut len = words
            .next()
            .and_then(|len| from_utf8(len).ok())
            .and_then(|len| len.parse::<usize>().ok())
            .unwrap_or(512);

          // just read from entropy for now
          let mut data = [0u8; 512];
          while len > 0 {
            let rem = len.min(512);
            virtio_entropy.read(&mut data[..rem]);
            match fs.write(fd, &data[..rem]) {
              Ok(written) if written == rem => {
                len -= rem;
              },
              Ok(written) => {
                let _ = writeln!(
                  uart,
                  "Failed to write full amount to {:?}, expected: {}, got: {}",
                  fd, rem, written,
                );
                break;
              },
              Err(e) => {
                let _ = writeln!(uart, "Failed to write to {:?}: {:?}", fd, e);
                break;
              },
            }
          }
        },
        b"fclose" => {
          let fd = words
            .next()
            .and_then(|fd| from_utf8(fd).ok())
            .and_then(|fd| fd.parse::<u32>().ok());
          let fd = if let Some(fd) = fd {
            fs::FileDescriptor::from(fd)
          } else {
            let _ = uart.write_str("Usage: fclose <file_descriptor>");
            continue;
          };
          match fs.close(fd) {
            Ok(()) => {},
            Err(e) => {
              let _ = writeln!(uart, "Failed to close {:?}: {:?}", fd, e);
              break;
            },
          };
        },
        b"fseek" => {
          let fd = words
            .next()
            .and_then(|fd| from_utf8(fd).ok())
            .and_then(|fd| fd.parse::<u32>().ok());
          let fd = if let Some(fd) = fd {
            fs::FileDescriptor::from(fd)
          } else {
            let _ = uart.write_str("Usage: fseek <file_descriptor> <from_start=0>");
            continue;
          };
          let seek_pos = words
            .next()
            .and_then(|len| from_utf8(len).ok())
            .and_then(|len| len.parse::<u32>().ok())
            .unwrap_or(0);
          match fs.seek(fd, fs::SeekFrom::Start(seek_pos)) {
            Ok(()) => {},
            Err(err) => {
              let _ = writeln!(uart, "Failed to seek for {:?}: {:?}", fd, err);
            },
          }
        },
        b"exit" => {
          if let Err(e) = fs.flush() {
            let _ = writeln!(uart, "Failed to flush: {:?}", e);
          }
          break;
        },
        b"fs_stat" => {
          let _ = writeln!(uart, "FS Stats: {:?}", fs.fs_stats());
        }
        _ => {
          let _ = writeln!(
            uart,
            "Unknown command \"{}\"",
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
