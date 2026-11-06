use ryo_core::errors::CompilerError;
use std::fmt;
use std::io::{self, Write};

pub fn write_stdout(args: fmt::Arguments) -> Result<(), CompilerError> {
    write_and_flush(io::stdout().lock(), args)
}

pub(crate) fn write_and_flush<W: Write>(
    mut out: W,
    args: fmt::Arguments,
) -> Result<(), CompilerError> {
    finish_output(out.write_fmt(args).and_then(|_| out.flush()))
}

pub(crate) fn finish_output(result: io::Result<()>) -> Result<(), CompilerError> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result.map_err(CompilerError::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct WriteError(io::ErrorKind);

    impl Write for WriteError {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.0, "write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_write_is_ignored() {
        write_and_flush(
            WriteError(io::ErrorKind::BrokenPipe),
            format_args!("output"),
        )
        .expect("closed pipe should be ignored");
    }

    #[test]
    fn other_write_errors_are_propagated() {
        let result = write_and_flush(WriteError(io::ErrorKind::Other), format_args!("output"));
        assert!(matches!(
            result,
            Err(CompilerError::IoError(error)) if error.kind() == io::ErrorKind::Other
        ));
    }

    struct FlushError;

    impl Write for FlushError {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    #[test]
    fn flush_errors_are_propagated() {
        let result = write_and_flush(FlushError, format_args!("output"));
        assert!(matches!(
            result,
            Err(CompilerError::IoError(error)) if error.kind() == io::ErrorKind::Other
        ));
    }
}
