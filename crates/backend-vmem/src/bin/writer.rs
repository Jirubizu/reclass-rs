//! Test target for the access tracker: repeatedly writes a known heap cell and
//! prints its address. Built only with the `access-tracker` feature.

use std::io::Write;

fn main() {
    let mut cell: Box<u64> = Box::new(0);
    let addr = &*cell as *const u64 as usize;
    println!("{addr:#x}");
    std::io::stdout().flush().unwrap();

    loop {
        // the write whose instruction the tracker should catch
        *cell = cell.wrapping_add(1);
        std::hint::black_box(&cell);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
