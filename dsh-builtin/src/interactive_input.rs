use dsh_types::Context;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader};

pub(crate) fn read_line(ctx: &Context, input: &mut String) -> io::Result<usize> {
    if ctx.interactive
        && let Ok(tty) = OpenOptions::new().read(true).open("/dev/tty")
    {
        let mut reader = BufReader::new(tty);
        return reader.read_line(input);
    }

    io::stdin().read_line(input)
}
