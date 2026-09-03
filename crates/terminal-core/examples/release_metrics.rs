use std::mem::size_of;
use std::time::Instant;

use terminal_core::{Cell, EndpointSize, TerminalState};

fn main() {
    let geometry = EndpointSize::new(120, 40).expect("benchmark geometry is valid");
    let scrollback_limit = 2_048;
    let mut terminal = TerminalState::new(geometry, scrollback_limit);
    let frame = b"\x1b[H\x1b[38;5;39mrelease-metric\x1b[0m\x1b[K\r\n";
    let repetitions = 20_000;
    let mut trace = Vec::with_capacity(frame.len() * repetitions);
    for _ in 0..repetitions {
        trace.extend_from_slice(frame);
    }

    let started = Instant::now();
    terminal.feed(&trace);
    let elapsed = started.elapsed();
    let elapsed_ns = elapsed.as_nanos().max(1);
    let trace_len = u128::try_from(trace.len()).expect("trace length fits u128");
    let bytes_per_second = trace_len * 1_000_000_000 / elapsed_ns;
    let mib_per_second = bytes_per_second / (1024 * 1024);
    let screen_rows = usize::from(geometry.rows) * 2;
    let retained_cell_bound = size_of::<Cell>()
        * usize::from(geometry.cols)
        * (screen_rows + scrollback_limit);

    assert!(terminal.scrollback_len() <= scrollback_limit);
    assert!(retained_cell_bound < 64 * 1024 * 1024);

    println!("terminal_core_feed_bytes={}", trace.len());
    println!("terminal_core_feed_elapsed_ns={elapsed_ns}");
    println!("terminal_core_feed_mib_per_second={mib_per_second}");
    println!("cell_size_bytes={}", size_of::<Cell>());
    println!("visible_grid_cells={}", usize::from(geometry.cols) * usize::from(geometry.rows));
    println!("screen_buffer_count=2");
    println!("scrollback_limit_rows={scrollback_limit}");
    println!("retained_cell_storage_bound_bytes={retained_cell_bound}");
}
