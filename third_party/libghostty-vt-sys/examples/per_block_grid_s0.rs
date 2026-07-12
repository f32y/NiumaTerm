use std::ffi::c_void;
use std::process::Command;
use std::{env, ptr};

use libghostty_vt_sys as vt;

struct Terminal(vt::Terminal);

impl Terminal {
    fn new(cols: u16, rows: u16, scrollback_bytes: usize) -> Self {
        let mut raw = ptr::null_mut();
        let result = unsafe {
            vt::ghostty_terminal_new(
                ptr::null(),
                &mut raw,
                vt::TerminalOptions {
                    cols,
                    rows,
                    max_scrollback: scrollback_bytes,
                },
            )
        };
        assert_eq!(result, vt::Result::SUCCESS);
        Self(raw)
    }

    fn write(&mut self, bytes: &[u8]) {
        unsafe { vt::ghostty_terminal_vt_write(self.0, bytes.as_ptr(), bytes.len()) };
    }

    fn rows(&self) -> usize {
        let mut rows = 0;
        let result = unsafe {
            vt::ghostty_terminal_get(
                self.0,
                vt::TerminalData::TOTAL_ROWS,
                (&mut rows as *mut usize).cast(),
            )
        };
        assert_eq!(result, vt::Result::SUCCESS);
        rows
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { vt::ghostty_terminal_free(self.0) };
    }
}

fn input(cols: u16, lines: usize, dense: bool) -> Vec<u8> {
    let width = usize::from(cols).saturating_sub(2);
    let mut bytes = Vec::with_capacity((width + 2) * lines);
    for line in 0..lines {
        if dense {
            let mut state = line as u64 ^ 0x9e37_79b9_7f4a_7c15;
            for _ in 0..width {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                bytes.push(33 + (state >> 32) as u8 % 94);
            }
        } else {
            bytes.extend_from_slice(format!("line-{line:08x}").as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
    }
    bytes
}

#[cfg(windows)]
fn private_bytes() -> usize {
    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            size: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCountersEx {
        cb: size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            size_of::<ProcessMemoryCountersEx>() as u32,
        )
    };
    assert_ne!(ok, 0);
    counters.private_usage
}

#[cfg(not(windows))]
fn private_bytes() -> usize {
    panic!("the S0 memory spike currently targets the checked-in Windows prebuilt")
}

fn run_case(shape: &str, cols: u16, lines: usize) {
    let before = private_bytes();
    let budget = lines * usize::from(cols) * 128;
    let mut terminal = Terminal::new(cols, 24, budget);
    let empty = private_bytes();
    terminal.write(&input(cols, lines, shape == "dense"));
    let loaded = private_bytes();
    let retained = loaded.saturating_sub(before);
    println!(
        "{shape},{cols},{lines},{},{},{},{},{:.1}",
        terminal.rows(),
        empty.saturating_sub(before),
        retained,
        loaded,
        retained as f64 / lines as f64,
    );
}

fn verify_two_terminal_freeze_model() {
    let before = private_bytes();
    let budget = 20_024 * 80 * 128;
    let mut frozen = Terminal::new(80, 24, budget);
    frozen.write(&input(80, 10_000, true));
    let frozen_rows = frozen.rows();
    let frozen_private = private_bytes();

    let two_terminal_private;
    {
        let mut active = Terminal::new(80, 24, budget);
        assert_eq!(active.rows(), 24);
        active.write(&input(80, 10_000, true));
        assert_eq!(frozen.rows(), frozen_rows);
        active.write(&input(80, 10_000, false));
        assert_eq!(frozen.rows(), frozen_rows);
        two_terminal_private = private_bytes();
    }
    assert_eq!(frozen.rows(), frozen_rows);

    eprintln!(
        "freeze-model: PASS; frozen_rows={frozen_rows}; frozen_private_delta={}; two_terminal_private_delta={}; frozen_survives_peer_free=true",
        frozen_private.saturating_sub(before),
        two_terminal_private.saturating_sub(before),
    );
}

fn main() {
    let args = env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "--case") {
        run_case(&args[2], args[3].parse().unwrap(), args[4].parse().unwrap());
        return;
    }

    verify_two_terminal_freeze_model();
    println!(
        "shape,cols,input_lines,total_rows,empty_terminal_bytes,retained_private_bytes,process_private_bytes,bytes_per_input_line"
    );
    let executable = env::current_exe().unwrap();
    for cols in [80, 120] {
        for lines in [1_000, 5_000, 10_000] {
            for shape in ["sparse", "dense"] {
                let output = Command::new(&executable)
                    .args(["--case", shape, &cols.to_string(), &lines.to_string()])
                    .output()
                    .unwrap();
                assert!(output.status.success());
                print!("{}", String::from_utf8(output.stdout).unwrap());
            }
        }
    }
}
