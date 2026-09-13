//! Writing the input line into the caller's frame buffer: the plain and
//! syntax-coloured paths (`print`/`write_colored_ranges_to`), the inline
//! completion suffix (`print_candidates`) and the colour accessors the REPL reads
//! back. Nothing here flushes -- see `print`'s note.
use super::*;

impl Input {
    /// Write the input line (and any ghost suffix) into `out`.
    ///
    /// `out` is the caller's frame buffer, not the terminal: it must not be
    /// flushed here. The redraw writes the clear sequence, the prompt mark, this
    /// line, the hint and the cursor restore into one buffer so the terminal sees
    /// a single atomic frame; flushing mid-way would split it across writes.
    pub fn print<W: Write>(&self, out: &mut W, ghost_suffix: Option<&str>) {
        if let Some(color_ranges) = &self.color_ranges {
            // Write colored segments directly to reduce allocation
            self.write_colored_ranges_to(out, color_ranges).ok();
        } else {
            for (i, line) in self.as_str().split('\n').enumerate() {
                if i > 0 {
                    out.write_all(b"\r\n").ok();
                }
                out.write_fmt(format_args!("{}", line.with(self.config.fg_color)))
                    .ok();
            }
        }

        if let Some(suffix) = ghost_suffix.filter(|s| !s.is_empty()) {
            for (i, line) in suffix.split('\n').enumerate() {
                if i > 0 {
                    out.write_all(b"\r\n").ok();
                }
                out.write_fmt(format_args!("{}", line.with(self.config.ghost_color)))
                    .ok();
            }
        }
    }

    /// Write colored string from color ranges directly to writer
    /// Note: color_ranges must be sorted by start position (ensured by compute_color_ranges)
    fn write_colored_ranges_to<W: Write>(
        &self,
        writer: &mut W,
        color_ranges: &[(usize, usize, ColorType)],
    ) -> std::io::Result<()> {
        use crossterm::style::Stylize;

        let input_str = self.as_str();
        let mut last_end = 0;

        // color_ranges is already sorted by start position in compute_color_ranges
        for &(start, end, color_type) in color_ranges {
            // Add any uncolored text before this range
            if start > last_end {
                let prefix = &input_str[last_end..start];
                for (i, line) in prefix.split('\n').enumerate() {
                    if i > 0 {
                        write!(writer, "\r\n")?;
                    }
                    write!(writer, "{}", line.with(self.config.fg_color))?;
                }
            }

            // Add the colored text for this range
            let colored_text = &input_str[start..end];
            let color = match color_type {
                ColorType::CommandExists => self.config.command_exists_color,
                ColorType::CommandNotExists => self.config.command_not_exists_color,
                ColorType::Argument => self.config.argument_color,
                ColorType::Variable => self.config.variable_color,
                ColorType::Assignment => self.config.assignment_color,
                ColorType::SingleQuote => self.config.single_quote_color,
                ColorType::DoubleQuote => self.config.double_quote_color,
                ColorType::Redirect => self.config.redirect_color,
                ColorType::Operator => self.config.operator_color,
                ColorType::Pipe => self.config.pipe_color,
                ColorType::Background => self.config.background_color,
                ColorType::ProcSubst => self.config.proc_subst_color,
                ColorType::Error => self.config.error_color,
                ColorType::ValidPath => self.config.valid_path_color,
                ColorType::HistoryMatch => self.config.history_match_fg_color,
            };

            for (i, line) in colored_text.split('\n').enumerate() {
                if i > 0 {
                    write!(writer, "\r\n")?;
                }
                if matches!(color_type, ColorType::HistoryMatch) {
                    write!(
                        writer,
                        "{}",
                        line.with(color).on(self.config.history_match_bg_color)
                    )?;
                } else {
                    write!(writer, "{}", line.with(color))?;
                }
            }

            // Update the last processed position
            last_end = end.max(last_end);
        }

        // Add any remaining uncolored text after the last range
        if last_end < input_str.len() {
            let suffix = &input_str[last_end..];
            for (i, line) in suffix.split('\n').enumerate() {
                if i > 0 {
                    write!(writer, "\r\n")?;
                }
                write!(writer, "{}", line.with(self.config.fg_color))?;
            }
        }

        Ok(())
    }

    pub fn fg_color(&self) -> Color {
        self.config.fg_color
    }

    pub fn command_exists_color(&self) -> Color {
        self.config.command_exists_color
    }

    pub fn command_not_exists_color(&self) -> Color {
        self.config.command_not_exists_color
    }

    pub fn argument_color(&self) -> Color {
        self.config.argument_color
    }

    pub fn completion_color(&self) -> Color {
        self.config.completion_color
    }

    pub fn ghost_color(&self) -> Color {
        self.config.ghost_color
    }

    /// Append the inline completion candidate to the caller's frame buffer.
    /// Like [`Input::print`], this must not flush — see that method's note.
    pub fn print_candidates<W: Write>(&mut self, out: &mut W, completion: String) {
        let current_byte = self.byte_index();
        let is_end = current_byte == self.input.len();

        out.write_fmt(format_args!(
            "{}",
            completion.with(self.config.completion_color)
        ))
        .ok();

        if !is_end {
            let tmp = &self.input[current_byte..];
            out.write_fmt(format_args!("{}", tmp.with(self.config.fg_color)))
                .ok();
        }
    }

    pub fn split_current_pos(&self) -> Option<(&str, &str)> {
        let current_byte = self.byte_index();
        if current_byte == self.input.len() {
            None
        } else {
            let pre = &self.input[..current_byte];
            let post = &self.input[current_byte..];
            Some((pre, post))
        }
    }
}
