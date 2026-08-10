//! Linux process identity, port allocation, signals, and readiness.
#![allow(dead_code)]

use std::fs;
use std::io;

use crate::state::Lease;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Liveness {
    Alive,
    Dead,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcIdentity {
    pgid: i32,
    start_time_ticks: u64,
}

pub(crate) fn lease_liveness(lease: &Lease) -> Liveness {
    match fs::read_to_string(format!("/proc/{}/stat", lease.pid)) {
        Ok(stat) => match parse_stat(&stat) {
            Some(identity)
                if identity.pgid == lease.pgid
                    && identity.start_time_ticks == lease.process_start_time_ticks =>
            {
                Liveness::Alive
            }
            Some(_) => Liveness::Dead,
            None => Liveness::Indeterminate,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Liveness::Dead,
        Err(_) => Liveness::Indeterminate,
    }
}

fn parse_stat(stat: &str) -> Option<ProcIdentity> {
    let command_end = stat.rfind(')')?;
    let fields: Vec<_> = stat.get(command_end + 1..)?.split_whitespace().collect();
    Some(ProcIdentity {
        pgid: fields.get(2)?.parse().ok()?,
        start_time_ticks: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::{ProcIdentity, parse_stat};

    #[test]
    fn parses_pgid_and_start_time_even_when_comm_contains_spaces_and_parentheses() {
        let stat =
            "42 (worker (test) name) S 1 77 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 21";
        assert_eq!(
            parse_stat(stat),
            Some(ProcIdentity {
                pgid: 77,
                start_time_ticks: 98765
            })
        );
    }

    #[test]
    fn malformed_or_truncated_proc_data_is_indeterminate() {
        assert_eq!(parse_stat("42 (worker) S 1"), None);
        assert_eq!(parse_stat("not proc stat"), None);
    }

    #[test]
    fn current_process_identity_is_readable_and_stable() {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
        let first = parse_stat(&stat).unwrap();
        let second = parse_stat(
            &std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap(),
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(first.start_time_ticks > 0);
    }
}
