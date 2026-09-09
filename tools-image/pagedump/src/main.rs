//! aenv-pagedump: dump a process's present guest-physical address ranges.
//!
//! Usage: `aenv-pagedump <pid>`
//!
//! Reads `/proc/<pid>/maps` and `/proc/<pid>/pagemap`, and prints the GPA
//! ranges of pages currently present in RAM as JSON:
//! `{"ranges":[[gpa,len],...]}` (sorted, coalesced). Used at snapshot
//! capture time to record a guest process's resident working set for
//! later bulk prefetch at resume.

use std::os::unix::fs::FileExt;
use std::process::ExitCode;

const PAGE_SIZE: u64 = 4096;
const PAGEMAP_ENTRY_SIZE: u64 = 8;
const PM_PRESENT: u64 = 1 << 63;
const PM_PFN_MASK: u64 = (1 << 55) - 1;
const CHUNK_PAGES: u64 = 65536; // 512 KiB of pagemap entries per read

fn main() -> ExitCode {
    let Some(pid) = std::env::args().nth(1) else {
        eprintln!("usage: aenv-pagedump <pid>");
        return ExitCode::from(2);
    };
    match run(&pid) {
        Ok(ranges) => {
            let body = ranges
                .iter()
                .map(|(gpa, len)| format!("[{gpa},{len}]"))
                .collect::<Vec<_>>()
                .join(",");
            println!("{{\"ranges\":[{body}]}}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("aenv-pagedump: {error}");
            ExitCode::from(1)
        }
    }
}

fn run(pid: &str) -> Result<Vec<(u64, u64)>, Box<dyn std::error::Error>> {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps"))?;
    let pagemap = std::fs::File::open(format!("/proc/{pid}/pagemap"))?;

    let mut out: Vec<(u64, u64)> = Vec::new();
    for line in maps.lines() {
        let Some((range, _)) = line.split_once(' ') else {
            continue;
        };
        let Some((start_s, end_s)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (
            u64::from_str_radix(start_s, 16),
            u64::from_str_radix(end_s, 16),
        ) else {
            continue;
        };
        if end <= start {
            continue;
        }
        let first_vpn = start / PAGE_SIZE;
        let npages = (end - start) / PAGE_SIZE;
        let mut done = 0u64;
        while done < npages {
            let take = CHUNK_PAGES.min(npages - done);
            let mut buf = vec![0u8; (take * PAGEMAP_ENTRY_SIZE) as usize];
            let offset = (first_vpn + done) * PAGEMAP_ENTRY_SIZE;
            // Entries that fail to read (e.g. special VMAs or truncated
            // reads) are treated as absent — this export is best-effort.
            if pagemap.read_exact_at(&mut buf, offset).is_ok() {
                for chunk in buf.chunks_exact(PAGEMAP_ENTRY_SIZE as usize) {
                    let entry = u64::from_le_bytes(chunk.try_into().expect("8-byte chunk"));
                    if entry & PM_PRESENT == 0 {
                        continue;
                    }
                    let gpa = (entry & PM_PFN_MASK) * PAGE_SIZE;
                    if let Some((last_start, last_len)) = out.last_mut() {
                        if gpa == *last_start + *last_len {
                            *last_len += PAGE_SIZE;
                            continue;
                        }
                    }
                    out.push((gpa, PAGE_SIZE));
                }
            }
            done += take;
        }
    }
    Ok(out)
}
