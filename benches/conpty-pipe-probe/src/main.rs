//! ConPTY throughput ceiling probe. Runs a command under the bundled ConPTY
//! with the conout side configured as an anonymous or named pipe of a chosen
//! buffer size, drains it with plain blocking ReadFile calls (no parsing, no
//! UI, no worker thread), and reports the read-size distribution, throughput,
//! and the child's final screen text (for vtebench summaries). The result is
//! the best any consumer can achieve on this machine and power plan; compare
//! terminal benchmarks against it before attributing a delta to terminal code.
//!
//! Usage: conpty-pipe-probe <anon|named> <pipe_kib> <read_kib> <cols> <rows> -- <cmdline...>
//! Env: PROBE_CONPTY_DLL overrides the conpty.dll path (default: the repo's
//! target/release/conpty.dll next to OpenConsole.exe); PROBE_SPIN=N spawns N
//! busy threads to measure the effect of a loaded process.
//! Run through scripts/conpty-pipe-probe.ps1.

use std::time::{Duration, Instant};
use std::{env, mem, process, ptr, thread};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, CreatePipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, EXTENDED_STARTUPINFO_PRESENT, INFINITE, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW, UpdateProcThreadAttribute, WaitForSingleObject,
};

type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> ! {
    let code = unsafe { GetLastError() };
    eprintln!("{what} failed: win32 error {code}");
    process::exit(2);
}

/// Returns (our read end, ConPTY write end).
unsafe fn make_conout(mode: &str, pipe_bytes: u32) -> (HANDLE, HANDLE) {
    unsafe {
        match mode {
            "anon" => {
                let mut read: HANDLE = ptr::null_mut();
                let mut write: HANDLE = ptr::null_mut();
                if CreatePipe(&mut read, &mut write, ptr::null(), pipe_bytes) == 0 {
                    last_error("CreatePipe");
                }
                (read, write)
            }
            "named" => {
                let name = wide(&format!(
                    r"\\.\pipe\nmt-conpty-probe-{}-{}",
                    process::id(),
                    Instant::now().elapsed().as_nanos()
                ));
                let server = CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1,
                    pipe_bytes,
                    pipe_bytes,
                    0,
                    ptr::null(),
                );
                if server == INVALID_HANDLE_VALUE {
                    last_error("CreateNamedPipeW");
                }
                let client = CreateFileW(
                    name.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                );
                if client == INVALID_HANDLE_VALUE {
                    last_error("CreateFileW(client)");
                }
                if ConnectNamedPipe(server, ptr::null_mut()) == 0
                    && GetLastError() != ERROR_PIPE_CONNECTED
                {
                    last_error("ConnectNamedPipe");
                }
                (server, client)
            }
            other => {
                eprintln!("unknown mode {other}");
                process::exit(2);
            }
        }
    }
}

fn strip_escapes(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            i += 1;
            match bytes.get(i) {
                Some(b'[') => {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                Some(b']') => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => i += 2,
                None => {}
            }
            continue;
        }
        if b == b'\r' {
            i += 1;
            continue;
        }
        if b == b'\n' || (0x20..0x7f).contains(&b) {
            out.push(b as char);
        }
        i += 1;
    }
    out
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    if split < 6 {
        eprintln!(
            "usage: conpty-pipe-probe <anon|named> <pipe_kib> <read_kib> <cols> <rows> -- <cmdline...>"
        );
        process::exit(2);
    }
    let mode = args[1].as_str();
    let pipe_bytes: u32 = args[2].parse::<u32>().unwrap() * 1024;
    let read_bytes: usize = args[3].parse::<usize>().unwrap() * 1024;
    let cols: i16 = args[4].parse().unwrap();
    let rows: i16 = args[5].parse().unwrap();
    let cmdline = args[split + 1..].join(" ");
    if cmdline.is_empty() {
        eprintln!("missing command line after --");
        process::exit(2);
    }

    // Optional busy threads keep cores out of deep idle states so the
    // pipe handoff latency can be compared against a quiet process.
    let spin: usize = env::var("PROBE_SPIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    for _ in 0..spin {
        thread::spawn(|| {
            let mut x = 0u64;
            loop {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                std::hint::black_box(x);
            }
        });
    }

    // The probe binary lives in benches/conpty-pipe-probe/target/<profile>/,
    // four levels below the repository root that holds target/release/conpty.dll.
    let dll = env::var("PROBE_CONPTY_DLL").unwrap_or_else(|_| {
        let exe = env::current_exe().expect("current exe path");
        exe.ancestors()
            .nth(4)
            .map(|root| root.join("target").join("release").join("conpty.dll"))
            .expect("repository root")
            .to_string_lossy()
            .into_owned()
    });

    unsafe {
        let module = LoadLibraryW(wide(&dll).as_ptr());
        if module.is_null() {
            last_error("LoadLibraryW(conpty.dll)");
        }
        let create: CreatePseudoConsoleFn = mem::transmute(
            GetProcAddress(module, c"ConptyCreatePseudoConsole".as_ptr().cast())
                .or_else(|| GetProcAddress(module, c"CreatePseudoConsole".as_ptr().cast()))
                .expect("CreatePseudoConsole export"),
        );
        let close: ClosePseudoConsoleFn = mem::transmute(
            GetProcAddress(module, c"ConptyClosePseudoConsole".as_ptr().cast())
                .or_else(|| GetProcAddress(module, c"ClosePseudoConsole".as_ptr().cast()))
                .expect("ClosePseudoConsole export"),
        );

        let mut conin_read: HANDLE = ptr::null_mut();
        let mut conin_write: HANDLE = ptr::null_mut();
        if CreatePipe(&mut conin_read, &mut conin_write, ptr::null(), 0) == 0 {
            last_error("CreatePipe(conin)");
        }
        let (conout_read, conout_write) = make_conout(mode, pipe_bytes);

        let mut hpc: HPCON = 0;
        let hr = create(
            COORD { X: cols, Y: rows },
            conin_read,
            conout_write,
            0,
            &mut hpc,
        );
        if hr != 0 {
            eprintln!("CreatePseudoConsole failed: HRESULT {hr:#010x}");
            process::exit(2);
        }
        CloseHandle(conin_read);
        CloseHandle(conout_write);

        let mut size: usize = 0;
        InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
        let mut attr_list = vec![0u8; size];
        let mut si: STARTUPINFOEXW = mem::zeroed();
        si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
        si.lpAttributeList = attr_list.as_mut_ptr().cast();
        if InitializeProcThreadAttributeList(si.lpAttributeList, 1, 0, &mut size) == 0 {
            last_error("InitializeProcThreadAttributeList");
        }
        if UpdateProcThreadAttribute(
            si.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const std::ffi::c_void,
            mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null_mut(),
        ) == 0
        {
            last_error("UpdateProcThreadAttribute");
        }

        let mut cmd = wide(&cmdline);
        let mut pi: PROCESS_INFORMATION = mem::zeroed();
        if CreateProcessW(
            ptr::null(),
            cmd.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT,
            ptr::null(),
            ptr::null(),
            &si.StartupInfo as *const STARTUPINFOW,
            &mut pi,
        ) == 0
        {
            last_error("CreateProcessW");
        }
        CloseHandle(pi.hThread);

        // Closing the pseudoconsole after the child exits ends the conout
        // stream, which terminates the read loop below with a broken pipe.
        let process_handle = pi.hProcess as usize;
        let hpc_addr = hpc as usize;
        thread::spawn(move || {
            WaitForSingleObject(process_handle as HANDLE, INFINITE);
            close(hpc_addr as HPCON);
        });

        let mut buf = vec![0u8; read_bytes];
        let mut tail: Vec<u8> = Vec::new();
        let mut reads: u64 = 0;
        let mut total: u64 = 0;
        let mut exact_4k: u64 = 0;
        let mut buckets = [0u64; 9]; // <=1K,<=2K,<=4K,<=8K,<=16K,<=32K,<=64K,<=128K,>128K
        let mut max_read = 0usize;
        let mut first: Option<Instant> = None;
        let mut last = Instant::now();
        let mut max_gap = Duration::ZERO;

        loop {
            let mut got: u32 = 0;
            let ok = ReadFile(
                conout_read,
                buf.as_mut_ptr(),
                read_bytes as u32,
                &mut got,
                ptr::null_mut(),
            );
            if ok == 0 || got == 0 {
                break;
            }
            let now = Instant::now();
            match first {
                None => first = Some(now),
                Some(_) => max_gap = max_gap.max(now - last),
            }
            last = now;
            let n = got as usize;
            reads += 1;
            total += n as u64;
            max_read = max_read.max(n);
            if n == 4096 {
                exact_4k += 1;
            }
            let idx = match n {
                0..=1024 => 0,
                1025..=2048 => 1,
                2049..=4096 => 2,
                4097..=8192 => 3,
                8193..=16384 => 4,
                16385..=32768 => 5,
                32769..=65536 => 6,
                65537..=131072 => 7,
                _ => 8,
            };
            buckets[idx] += 1;
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > 4 << 20 {
                let keep = tail.len() - (2 << 20);
                tail.drain(..keep);
            }
        }
        CloseHandle(conout_read);
        CloseHandle(conin_write);

        let elapsed = first.map_or(Duration::ZERO, |f| last - f);
        let mib = total as f64 / 1_048_576.0;
        println!(
            "mode={mode} pipe={}KiB read_buf={}KiB grid={cols}x{rows} spin={spin}",
            pipe_bytes / 1024,
            read_bytes / 1024
        );
        println!(
            "reads={reads} bytes={total} ({mib:.1} MiB) elapsed={:.2}s throughput={:.1} MiB/s avg_read={:.0}B max_read={max_read}B exact_4096={exact_4k} max_gap={:.1}ms",
            elapsed.as_secs_f64(),
            mib / elapsed.as_secs_f64().max(1e-9),
            total as f64 / reads.max(1) as f64,
            max_gap.as_secs_f64() * 1000.0
        );
        let labels = [
            "<=1K", "<=2K", "<=4K", "<=8K", "<=16K", "<=32K", "<=64K", "<=128K", ">128K",
        ];
        let hist: Vec<String> = labels
            .iter()
            .zip(buckets.iter())
            .filter(|(_, c)| **c > 0)
            .map(|(l, c)| format!("{l}:{c}"))
            .collect();
        println!("read_size_hist {}", hist.join(" "));
        let text = strip_escapes(&tail);
        for line in text.lines() {
            let t = line.trim();
            if t.contains("samples") || t.contains("avg") {
                println!("child: {t}");
            }
        }
    }
}
