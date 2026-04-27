//! File list building, transmission, reception and sorting.
//!
//! Corresponds to rsync's flist.c.

pub mod recv;
pub mod send;
pub mod sort;

pub use recv::{recv_file_list, recv_file_list_ex};
pub use send::{send_file_list, send_file_list_ex, Preserve};
pub use sort::flist_sort;
