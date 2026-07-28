//! Test target for the live-read and pointer-scan integration tests. Allocates
//! known sentinels, prints their addresses + values on one line, then waits to
//! be read/killed.
//!
//! Output: `<heap_addr> <heap_val> <arr_addr> <a0> <a1> <a2> <a3> <ptr_cell_addr>`

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A *static* slot holding the address of `arr`.
///
/// Lives in the binary's own mapping, so it is the module-backed root a
/// pointer scan must terminate on. Without it every pointer to `arr` is on the
/// heap, which moves every run and is correctly not reported as a static path.
static ARR_PTR: AtomicUsize = AtomicUsize::new(0);

fn main() {
    let heap: Box<u64> = Box::new(0x0011_2233_4455_6677);
    let arr: Box<[u32; 4]> = Box::new([0xAAAA_0001, 0xAAAA_0002, 0xAAAA_0003, 0xAAAA_0004]);

    let heap_addr = &*heap as *const u64 as usize;
    let arr_addr = arr.as_ptr() as usize;
    ARR_PTR.store(arr_addr, Ordering::SeqCst);

    // a heap cell whose value is the address of `arr`: a live ClassPtr target.
    let parent: Box<u64> = Box::new(arr_addr as u64);
    let parent_addr = &*parent as *const u64 as usize;

    println!(
        "{:#x} {} {:#x} {} {} {} {} {:#x}",
        heap_addr, *heap, arr_addr, arr[0], arr[1], arr[2], arr[3], parent_addr
    );
    std::io::stdout().flush().unwrap();

    // keep allocations alive and stay around for the reader
    std::thread::sleep(std::time::Duration::from_secs(20));
    std::hint::black_box((&heap, &arr, &parent, ARR_PTR.load(Ordering::SeqCst)));
}
