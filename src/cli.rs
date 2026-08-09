//! Command-line parsing, terminal output, and exit-code policy.
//!
//! NOOK-10 provides only the executable boundary and a useful startup message.
//! The complete command contract belongs to NOOK-11.

use std::io::{self, Write};

const INFORMATION: &str = "\
Nook — stable *.localhost domains for local services

The command interface is not implemented yet.
The complete command contract will be implemented in the next milestone.
";

pub(crate) fn run() -> crate::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    write_information(&mut output)
}

fn write_information(mut output: impl Write) -> crate::Result<()> {
    output.write_all(INFORMATION.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_information;

    #[test]
    fn startup_information_identifies_nook_and_its_current_scope() {
        let mut output = Vec::new();

        write_information(&mut output).expect("startup information should be writable");

        let output = String::from_utf8(output).expect("startup information should be UTF-8");
        assert!(output.contains("Nook"));
        assert!(output.contains("command interface is not implemented yet"));
    }
}
