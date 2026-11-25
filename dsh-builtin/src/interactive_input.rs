use dsh_types::Context;
use std::fs::File;
use std::io::{self, BufRead, BufReader};

pub(crate) fn read_line(ctx: &Context, input: &mut String) -> io::Result<usize> {
    if let Some(tty) = terminal_input(ctx) {
        let mut reader = BufReader::new(tty);
        return reader.read_line(input);
    }

    io::stdin().read_line(input)
}

pub(crate) fn terminal_input(ctx: &Context) -> Option<File> {
    ctx.interactive
        .then(|| File::open("/dev/tty").ok())
        .flatten()
}
